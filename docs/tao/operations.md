# How we run SuperCI

State on 2026-10-29. Owner: the CAFE CI work (`my-remotely/cafe-ci`, epic CFCI-1).

## What it does for us

```
push to develop (web-app)
  └─ GitHub Actions CI, 3 matrix jobs "G3 shard i/3"
       runs-on: superci/image=web-app-ci/cpu=8/ram=16
         └─ SuperCI control plane on Modal (app superci-plane-o6tkf7i1a8fc)
              picks a pool: modal → modal:info-fp → modal:info-mr
              starts a GitHub JIT runner as a Modal Function (preemptible)
                 image: ghcr.io/my-remotely/web-app-ci:<tag> (alias web-app-ci)
  └─ CD on remo-dev waits for green CI on the same SHA, then deploys dev
```

The dashboard is at https://ci.myremotely.ai/. Only the tailnet can reach it.

## Branches and PRs

`tao` = upstream `main` + the branches below + our docs.

| Branch | Upstream | What it gives |
|---|---|---|
| `fix/modal-libmodal-version` | PR #2 | Modal deploys send the `x-modal-libmodal-version` header (`modal-go/v0.7.3`). Sign-in polls `TokenFlowWait` in the background, so a sign-in is not lost. |
| `feat/modal-runner-image` | PR #4 | A label `superci/image=<alias>` starts the job from your own image. `superci runner_images create\|update\|delete`. |
| `feat/modal-workspaces` | PR #6 | Several Modal workspaces, each a pool `modal:<workspace>` with its own monthly limit. `superci login modal --add`. |
| `fix/modal-client-1.6.3` | PR #7 | Deploys use `modal==1.6.3`. Workspaces on image builder 2026.10 need it. |
| `feat/modal-function-mode` | PR #9, issue #8 | `superci runners update modal --mode=function\|sandbox`. Jobs run as preemptible Functions at 1/3 of the Sandbox price. A preempted job runs again once, on a Sandbox. |
| `fix/fast-status` (3eb4878, f822d41, c356926) | no PR yet | Own image in a further workspace (#4 × #6). `/status` reads Dict records in one batch. The CLI waits 30 s for `/status`. |
| `fix/modal-status-reads` | to open | The fast-status change rebased on upstream `main`. |

Issue #3: the dashboard on a remote server cannot finish sign-ins, because the links point to `localhost`.

To update `tao`:

```bash
git fetch upstream origin
git checkout tao
git merge upstream/main          # then each open PR branch that changed
git push origin tao
```

## Build and deploy on remo-dev

The source checkout is `/opt/superci/src` on `tao`. The CLI is `/usr/local/bin/superci`, and the previous binary stays as `superci.prev`. Config and keys are in `/root/.superci`, copied from MacMax.

```bash
ssh remo-dev
. $HOME/.cargo/env; cd /opt/superci/src
git fetch origin && git checkout -B tao origin/tao
nice cargo build --release --target x86_64-unknown-linux-musl -p superci-plane-modal
cp target/x86_64-unknown-linux-musl/release/superci-plane crates/plane-modal/build/superci-plane
nice cargo build --release -p superci
cp /usr/local/bin/superci /usr/local/bin/superci.prev
install -m 755 target/release/superci /usr/local/bin/superci
systemctl restart superci-dashboard
export HOME=/root
superci planes update                        # needed after a plane change; else the dashboard gives 409
superci runners create modal                 # refreshes launchers and job Functions
superci runners create modal --account=info-fp
superci runners create modal --account=info-mr
```

- Do the deploy when no CI job runs. Check with `gh run list --repo my-remotely/web-app --workflow CI --limit 1`.
- The CLI embeds the planes. Without the musl plane copied to `crates/plane-modal/build/`, `planes update` deploys an old plane.
- On the Mac, a new worktree has no embedded artifacts. Copy `crates/programs-{planes,cache,layers}/embedded` from this checkout, or run `./build.sh`.
- An old npm `superci` was once in `/root/.local/bin` and came first in `PATH`. It deployed an old plane. Check `which -a superci`.

The hosting files (Caddy, systemd units, Tailscale node `ci`, dashboard wrapper) live in `my-remotely/cafe-ci` under `ops/remo-dev/superci/`. The path is:

```
tailnet → Tailscale userspace node "ci" (tailscaled-ci, serve --tcp=443 --proxy-protocol=2)
        → caddy-ci 127.0.0.1:8443 (DNS-01 via Cloudflare, tailnet allowlist, adds the superci_local cookie)
        → superci dashboard 127.0.0.1:8976 (systemd superci-dashboard)
```

## Pools, routing and money

| Pool | Workspace | Limit | Mode |
|---|---|---|---|
| `modal` | artile | $25/month | function |
| `modal:info-fp` | info-fp | $25/month | function |
| `modal:info-mr` | info-mr | $25/month | function |

Each Starter workspace has $30/month of credit. The limit stays at $25, so we do not pay. When one pool reaches its limit, SuperCI uses the next. When all three reach it, jobs wait. Ignore the workspace `info-8758`: it is an old renamed account.

Modal prices (per second):

| Kind | CPU, per core | RAM, per GiB |
|---|---|---|
| Sandbox | $0.00003942 | $0.00000667 |
| Function (preemptible) | $0.0000131 | $0.00000222 |
| Function `nonpreemptible=True` | 3× | 3× |

- One Modal core = 2 vCPU. The label `cpu=8` means 8 vCPU. The launcher asks for 4 cores, and `nproc` in the job shows 4.
- RAM goes from 1 to 336 GB in 1 GB steps. Without a `ram` key, the default is 4 GB per CPU.
- One web-app push = 3 jobs × ~8 min at 4 cores and 16 GiB. That is ≈ $0.13 on Functions and ≈ $0.33 on Sandboxes. These are estimates; check the Modal bill.
- For comparison, AWS spot c8a.xlarge (4 vCPU, 8 GB) costs ≈ $0.0014/min. A Modal Sandbox costs ≈ 5.6× as much.
- `superci status` shows `Preempted: N Modal Functions this month · M jobs run again`.

## The web-app side

- `ci.yml`: `runs-on: ${{ fromJSON(vars.CI_RUNS_ON || '"superci/image=web-app-ci/cpu=8"') }}`. The repo variable `CI_RUNS_ON` is `"superci/image=web-app-ci/cpu=8/ram=16"`. To go back to the Mac runners, set it to `["self-hosted","mac"]`.
- Shards: `mise run gate3 -- --shard i/3`. Weights are in `aux/tooling/gates/g3-shard-weights.toml`. The tail of the work goes through a claim queue in a Modal Dict `g3-<sha12>-<run>` (`put(claim:k, shard, skip_if_exists=True)`). If Modal is not reachable, a shard takes `k mod n`. `mise run gate3:report` merges the shard reports.
- Failures stream as lines `G3-FAIL shard=… kind=check|test …`.
- The DB stage in shard 3 (native MariaDB, DB-state suites, golden join) runs only when `vars.CI_DB_STAGE == 'on'`. It is off now (temporary).
- `concurrency: ci-<workflow>-<PR or ref>`, `cancel-in-progress: true`. A new push to develop cancels the CI that runs for develop. Other refs run in parallel.
- The runner image is built on remo-dev by `aux/environments/ci/runner-image` and the daily workflow `g3-runner-image.yml`. Its tag is a hash of its inputs. Then `superci runner_images update web-app-ci --reference=ghcr.io/my-remotely/web-app-ci:<tag>` must run in each workspace. A new tag is needed when mise tools, `bun.lock`, the MariaDB or redis version, or the Dockerfile change.
- `aux/environments/ci/cloud.md` in web-app lists all cloud objects.

## Facts we learned

- **Modal sandboxes run on gVisor.** There is no cgroup v2 memory file. Read memory from `/proc/meminfo`: used = MemTotal − MemAvailable, and MemTotal is the RAM limit. This value may include page cache.
- **Peak RAM per shard** (run 40781833945): 4.4 GB, 14.1 GB and 15.6 GB of 17.2 GB. Do not go below `ram=16`.
- **MariaDB on Linux** needs `lower_case_table_names=0`. With `1` the migrations fail with errno 121 (duplicate foreign key name). macOS keeps `1`.
- **Nothing starts Redis on 6379 in the image.** The Mac runners had a brew service. The DB-state suites (Worker-parity tests) fail with `ECONNREFUSED 6379` without it.
- **`/status` was slow** because the Modal plane read ~85 Dict records one by one (~115 ms each). The CLI stopped at 8 s and printed `status: null` after 108 s. A batched `get_many` and a 30 s client limit fixed it.
- **Sign-in from a remote dashboard** fails (issue #3). Sign in with `superci dashboard` on the Mac, then copy `~/.superci` to `/root/.superci` on remo-dev.
- **Modal SDK versions.** A Python client older than 1.6 cannot open the newer (V2) sandboxes: `This request uses the V1 sandbox API`. Use `uv run --with modal==1.6.3`. On macOS Python, set `SSL_CERT_FILE` to the certifi bundle, or TLS fails with `CERTIFICATE_VERIFY_FAILED`.
- **Look inside a running job.** Find the machine id with `superci status --json` (`sb-…` = Sandbox, `fc-…` = Function). For a Sandbox, `modal.Sandbox.from_id(id).exec("bash","-c", …)` works. The live step output is in `/home/runner/_diag/pages/*.log` until the job ends. GitHub gives job logs only after the job ends.
- **Tar from macOS** adds AppleDouble `._*` files. Use `COPYFILE_DISABLE=1 tar --no-mac-metadata`.
- **The web-app pre-push hook runs G2** even for a branch delete. To delete a merged remote branch, use `git push --no-verify origin --delete <branch>`.

## Open items

- Upstream PRs for `fix/modal-status-reads` and 3eb4878 (#4 × #6).
- Start Redis in shard 3, then turn `CI_DB_STAGE` on again.
- Find what uses 14–15 GB in shards 2 and 3.
- Check the real cost of Function mode in the Modal bill.
