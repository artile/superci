//! `image-reader pack`: an image as pieces with their checksums, the hot list and the reader program beside them, in a
//! directory named by the index's own checksum; and what it refuses.
use std::process::Command;

fn dir(what: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("image-reader-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn pack(args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_image-reader")).arg("pack").args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
}

fn sha256(b: &[u8]) -> String { use sha2::Digest; sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect() }

#[test]
fn an_image_is_packed_in_pieces_with_its_hot_list_and_reader_under_its_indexs_checksum() {
    let d = dir("pack");
    let image: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(d.join("image.sqsh"), &image).unwrap();
    std::fs::write(d.join("used.json"), "[3, 1, 3, 99, 0]").unwrap();
    std::fs::write(d.join("reader"), b"the reader program").unwrap();
    let out = d.join("out");
    let (ok, said) = pack(&[d.join("image.sqsh").to_str().unwrap(), out.to_str().unwrap(), "--piece", "65536", "--hot", d.join("used.json").to_str().unwrap(), "--reader", d.join("reader").to_str().unwrap()]);
    assert!(ok && said.contains("4 pieces of 65536 bytes"), "{said}");
    let pin = said.lines().next().unwrap().rsplit('/').next().unwrap().to_string();
    let at = out.join(&pin);
    let index = std::fs::read(at.join("index.json")).unwrap();
    // Named by its index's checksum; each piece's checksum in it; together, the image.
    assert_eq!(&sha256(&index)[..32], pin);
    let v: serde_json::Value = serde_json::from_slice(&index).unwrap();
    assert_eq!((v["name"].as_str(), v["size"].as_u64(), v["piece"].as_u64()), (Some("image.sqsh"), Some(200_000), Some(65536)), "{v}");
    let mut whole = vec![];
    for i in 0..4 {
        let p = std::fs::read(at.join(format!("p/{i:06}"))).unwrap();
        assert_eq!(v["sha256"][i].as_str().unwrap(), sha256(&p));
        whole.extend(p);
    }
    assert_eq!(whole, image);
    // The hot list: in its order, each once, only pieces the image has; the reader beside, each by its checksum.
    let hot = std::fs::read(at.join("hot.json")).unwrap();
    assert_eq!(serde_json::from_slice::<Vec<u64>>(&hot).unwrap(), vec![3, 1, 0]);
    assert_eq!(v["hot"].as_str().unwrap(), sha256(&hot));
    assert_eq!(v["readers"]["x86_64"].as_str().unwrap(), sha256(b"the reader program"));
    assert_eq!(std::fs::read(at.join("reader-x86_64")).unwrap(), b"the reader program");
    // Packed again: the same directory, nothing left of the work.
    let (ok, again) = pack(&[d.join("image.sqsh").to_str().unwrap(), out.to_str().unwrap(), "--piece", "65536", "--hot", d.join("used.json").to_str().unwrap(), "--reader", d.join("reader").to_str().unwrap()]);
    assert!(ok && again.lines().next() == said.lines().next() && !out.join(".packing").exists(), "{again}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn what_pack_refuses_is_said() {
    let d = dir("refuse");
    std::fs::write(d.join("empty.img"), b"").unwrap();
    std::fs::write(d.join("small.img"), vec![1u8; 70_000]).unwrap();
    std::fs::write(d.join("bad.json"), "not a list").unwrap();
    let out = d.join("out");
    let (o, small, empty, bad) = (out.to_str().unwrap(), d.join("small.img"), d.join("empty.img"), d.join("bad.json"));
    for (args, said) in [
        (vec![], "image-reader pack <image file> <output directory>"),
        (vec![small.to_str().unwrap(), o, "--piece", "many"], "--piece: a number of bytes"),
        (vec![small.to_str().unwrap(), o, "--piece", "1000"], "pieces of 1000 bytes (64 KiB to 64 MiB)"),
        (vec![empty.to_str().unwrap(), o, "--piece", "65536"], "an image of 0 bytes"),
        (vec![d.join("none.img").to_str().unwrap(), o], "none.img"),
        (vec![small.to_str().unwrap(), o, "--piece", "65536", "--hot", bad.to_str().unwrap()], "bad.json"),
        (vec![small.to_str().unwrap(), o, "--piece", "65536", "--reader", d.join("no-reader").to_str().unwrap()], "no-reader"),
    ] {
        let (ok, got) = pack(&args);
        assert!(!ok && got.contains(said), "{args:?}: {got}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn showing_an_image_is_for_linux_only() {
    let o = Command::new(env!("CARGO_BIN_EXE_image-reader")).args(["https://example.com/x", "/tmp/m", "/tmp/c"]).output().unwrap();
    assert!(o.status.code() == Some(2) && String::from_utf8_lossy(&o.stderr).contains("on Linux only"), "{}", String::from_utf8_lossy(&o.stderr));
}
