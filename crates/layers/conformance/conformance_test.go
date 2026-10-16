// containerd's own conformance suites for snapshotters and content stores, run against superci-layers: the binary
// (SUPERCI_LAYERS, built for this system) started for each test on a folder of its own, reached through
// containerd's proxy clients as containerd reaches it; each suite also through containerd's metadata layer, as
// containerd itself uses a store. Linux, as root (the snapshotter suite mounts what it is given).
package conformance

import (
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	contentapi "github.com/containerd/containerd/api/services/content/v1"
	snapshotsapi "github.com/containerd/containerd/api/services/snapshots/v1"
	"github.com/containerd/containerd/v2/core/content"
	contentproxy "github.com/containerd/containerd/v2/core/content/proxy"
	contentsuite "github.com/containerd/containerd/v2/core/content/testsuite"
	"github.com/containerd/containerd/v2/core/metadata"
	"github.com/containerd/containerd/v2/core/snapshots"
	snapshotproxy "github.com/containerd/containerd/v2/core/snapshots/proxy"
	snapshotsuite "github.com/containerd/containerd/v2/core/snapshots/testsuite"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/pkg/labels"
	bolt "go.etcd.io/bbolt"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// start runs superci-layers on its own folders under root; what to close when the test is done.
func start(root string, args ...string) (*grpc.ClientConn, func() error, error) {
	bin := os.Getenv("SUPERCI_LAYERS")
	if bin == "" {
		return nil, nil, fmt.Errorf("SUPERCI_LAYERS (the binary) is not set")
	}
	sock := filepath.Join(root, "layers.sock")
	cmd := exec.Command(bin, append([]string{"--root=" + filepath.Join(root, "layers"), "--content=" + filepath.Join(root, "content"), "--socket=" + sock}, args...)...)
	cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
	if err := cmd.Start(); err != nil {
		return nil, nil, err
	}
	for i := 0; ; i++ {
		if c, err := net.Dial("unix", sock); err == nil {
			c.Close()
			break
		}
		if i > 200 {
			cmd.Process.Kill()
			return nil, nil, fmt.Errorf("superci-layers did not answer on %s", sock)
		}
		time.Sleep(25 * time.Millisecond)
	}
	conn, err := grpc.NewClient("unix://"+sock, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		cmd.Process.Kill()
		return nil, nil, err
	}
	return conn, func() error {
		conn.Close()
		cmd.Process.Signal(syscall.SIGTERM)
		done := make(chan error, 1)
		go func() { done <- cmd.Wait() }()
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			cmd.Process.Kill()
			return fmt.Errorf("superci-layers did not stop on SIGTERM")
		}
		return nil
	}, nil
}

func snapshotter(args ...string) snapshotsuite.SnapshotterFunc {
	return func(ctx context.Context, root string) (snapshots.Snapshotter, func() error, error) {
		conn, stop, err := start(root, args...)
		if err != nil {
			return nil, nil, err
		}
		sn := snapshotproxy.NewSnapshotter(snapshotsapi.NewSnapshotsClient(conn), "superci")
		return sn, func() error { sn.Close(); return stop() }, nil
	}
}

func TestSnapshotter(t *testing.T) {
	snapshotsuite.SnapshotterSuite(t, "overlayfs", snapshotter())
}

func TestSnapshotterRemovingAtOnce(t *testing.T) {
	snapshotsuite.SnapshotterSuite(t, "overlayfs", snapshotter("--sync-remove"))
}

// Through containerd's metadata layer: its namespaces, leases and names over the store's own.
func TestSnapshotterInMetadata(t *testing.T) {
	snapshotsuite.SnapshotterSuite(t, "overlayfs", func(ctx context.Context, root string) (snapshots.Snapshotter, func() error, error) {
		conn, stop, err := start(root)
		if err != nil {
			return nil, nil, err
		}
		db, err := bolt.Open(filepath.Join(root, "metadata.db"), 0660, nil)
		if err != nil {
			return nil, nil, err
		}
		cs, err := local.NewStore(filepath.Join(root, "metadata-content"))
		if err != nil {
			return nil, nil, err
		}
		mdb := metadata.NewDB(db, cs, map[string]snapshots.Snapshotter{"superci": snapshotproxy.NewSnapshotter(snapshotsapi.NewSnapshotsClient(conn), "superci")})
		if err := mdb.Init(ctx); err != nil {
			return nil, nil, err
		}
		return mdb.Snapshotter("superci"), func() error { db.Close(); return stop() }, nil
	})
}

func contentStore(ctx context.Context, root string) (context.Context, content.Store, func() error, error) {
	conn, stop, err := start(root)
	if err != nil {
		return nil, nil, nil, err
	}
	return ctx, contentproxy.NewContentStore(contentapi.NewContentClient(conn)), stop, nil
}

func TestContent(t *testing.T) {
	contentsuite.ContentSuite(t, "superci", contentStore)
}

// Through containerd's metadata layer, as containerd's own metadata tests run its local store: shared and isolated
// namespaces too.
func metadataContent(opts ...metadata.DBOpt) contentsuite.StoreInitFn {
	return func(ctx context.Context, root string) (context.Context, content.Store, func() error, error) {
		_, cs, stop, err := contentStore(ctx, root)
		if err != nil {
			return nil, nil, nil, err
		}
		db, err := bolt.Open(filepath.Join(root, "metadata.db"), 0660, nil)
		if err != nil {
			return nil, nil, nil, err
		}
		var (
			count atomic.Uint64
			name  = contentsuite.Name(ctx)
		)
		wrap := func(ctx context.Context, sharedNS bool) (context.Context, func(context.Context) error, error) {
			n := count.Add(1)
			ctx2 := namespaces.WithNamespace(ctx, fmt.Sprintf("%s-n%d", name, n))
			if sharedNS {
				db.Update(func(tx *bolt.Tx) error {
					if ns, err := namespaces.NamespaceRequired(ctx2); err == nil {
						return metadata.NewNamespaceStore(tx).SetLabel(ctx2, ns, labels.LabelSharedNamespace, "true")
					}
					return err
				})
			}
			return ctx2, func(context.Context) error { return nil }, nil
		}
		ctx = contentsuite.SetContextWrapper(ctx, wrap)
		return ctx, metadata.NewDB(db, cs, nil, opts...).ContentStore(), func() error { db.Close(); return stop() }, nil
	}
}

func TestContentInMetadata(t *testing.T) {
	contentsuite.ContentSuite(t, "metadata", metadataContent())
	contentsuite.ContentCrossNSSharedSuite(t, "metadata", metadataContent())
	contentsuite.ContentCrossNSIsolatedSuite(t, "metadata", metadataContent(metadata.WithPolicyIsolated))
	contentsuite.ContentSharedNSIsolatedSuite(t, "metadata", metadataContent(metadata.WithPolicyIsolated))
}
