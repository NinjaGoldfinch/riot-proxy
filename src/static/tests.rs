#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cmp::Ordering;

use super::*;
use crate::config::{Config, Sources};
use crate::jobs::ddragon::{Cdn, CdnUrls};

fn mirror(dir: &Path) -> Mirror {
    let config = Config::from_sources(Sources {
        env: vec![("RIOT_API_KEY".into(), "RGAPI-test-key-not-real".into())],
        ..Sources::default()
    })
    .unwrap();
    Mirror::new(
        dir,
        Cdn::new(&config, CdnUrls::mock("http://127.0.0.1:9")).unwrap(),
    )
}

/// A patch directory: `files` as given, and `versions.json` when `complete`.
fn patch(root: &Path, version: &str, files: &[(&str, &str)], complete: bool) {
    let dir = root.join(version);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(format!("{name}.json")), body).unwrap();
    }
    if complete {
        std::fs::write(dir.join("versions.json"), format!(r#"["{version}"]"#)).unwrap();
    }
}

#[test]
fn patches_compare_numerically_segment_by_segment() {
    // v1's cases: string order would put 9.24.1 above 16.17.1.
    assert_eq!(compare_versions("16.17.1", "9.24.1"), Ordering::Greater);
    assert_eq!(compare_versions("9.24.1", "16.17.1"), Ordering::Less);
    assert_eq!(compare_versions("16.17.1", "16.17.1"), Ordering::Equal);
    assert_eq!(compare_versions("16.2.1", "16.17.1"), Ordering::Less);
    // A missing segment is zero, not smaller.
    assert_eq!(compare_versions("16.17", "16.17.0"), Ordering::Equal);
    assert_eq!(compare_versions("16.17", "16.17.1"), Ordering::Less);
}

#[test]
fn only_patch_numbers_are_versions() {
    for ok in ["16.17.1", "16", "0.1"] {
        assert!(is_version(ok), "{ok}");
    }
    for bad in [
        "",
        "..",
        "16..1",
        "16.17.",
        ".16",
        "meta",
        "16.17.1/..",
        "lolpatch_7.20",
        "1e3",
    ] {
        assert!(!is_version(bad), "{bad}");
    }
}

#[test]
fn aliases_name_only_mirrored_files_and_every_file_has_one() {
    // v1 #52: an alias named a file the sync never mirrored.
    for (alias, file) in FILE_ALIASES {
        assert!(DATA_FILES.contains(&file), "{alias} → {file}");
        assert_eq!(resolve_file(alias), Some(file));
    }
    for file in DATA_FILES {
        assert!(
            FILE_ALIASES.iter().any(|(_, f)| *f == file),
            "{file} has no alias"
        );
        assert_eq!(resolve_file(file), Some(file));
    }
    assert_eq!(
        resolve_file("queue"),
        None,
        "Data Dragon has no queue.json (v1 #52)"
    );
    assert_eq!(resolve_file("versions"), None);
}

#[tokio::test]
async fn reads_a_mirrored_file_and_refuses_to_leave_the_mirror() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("ddragon");
    patch(&root, "16.17.1", &[("champion", r#"{"type":"champion"}"#)], true);
    // A real readable file outside the mirror, so "outside" is not just missing.
    std::fs::write(base.path().join("secret.json"), r#"{"probe":"leaked"}"#).unwrap();
    let m = mirror(&root);

    assert_eq!(
        m.read("champion", Some("16.17.1")).await.unwrap(),
        br#"{"type":"champion"}"#
    );
    assert_eq!(m.read("champion", None).await.unwrap(), br#"{"type":"champion"}"#);
    assert!(m.read("champion", Some("1.2.3")).await.is_none(), "never synced");
    // v1 #51: both segments are joined, so both are checked.
    for (file, version) in [
        ("secret", ".."),
        ("secret", "16.17.1/.."),
        ("champion", "../../../../../../etc"),
        ("../../secret", "16.17.1"),
    ] {
        assert!(m.read(file, Some(version)).await.is_none(), "{file} @ {version}");
    }
    assert!(m.read_meta("../secret").await.is_none());
}

#[tokio::test]
async fn the_current_patch_is_the_newest_complete_one_on_disk() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("ddragon");
    let m = mirror(&root);
    assert_eq!(m.current_version().await, None, "no mirror yet");

    patch(&root, "9.24.1", &[], true);
    patch(&root, "16.17.1", &[], true);
    patch(&root, "10.5.2", &[], true);
    // A sync that died half way: no versions.json, so not current.
    patch(&root, "16.18.1", &[("champion", "{}")], false);
    std::fs::create_dir_all(root.join(META_DIR)).unwrap();
    assert_eq!(m.current_version().await.as_deref(), Some("16.17.1"));

    // Remembered, not re-read: a sync moves it explicitly.
    patch(&root, "16.19.1", &[], true);
    assert_eq!(m.current_version().await.as_deref(), Some("16.17.1"));
    m.set_current("16.19.1");
    assert_eq!(m.current_version().await.as_deref(), Some("16.19.1"));
}

#[test]
fn champion_json_parses_to_id_and_name() {
    let names = champions::parse(
        br#"{"type":"champion","data":{
            "Ahri":{"key":"103","name":"Ahri"},
            "MonkeyKing":{"key":"62","name":"Wukong"},
            "Broken":{"key":"x","name":"Nope"},
            "Nameless":{"key":"1"}}}"#,
    );
    assert_eq!(names.len(), 2);
    assert_eq!(names[&103], "Ahri");
    assert_eq!(names[&62], "Wukong");
    assert!(champions::parse(b"not json").is_empty());
}

#[tokio::test]
async fn champion_names_follow_the_current_patch() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("ddragon");
    let file = |name: &str| {
        format!(
            r#"{{"data":{{"Ahri":{{"key":"103","name":"{name}"}},"Garen":{{"key":"86","name":"Garen"}}}}}}"#
        )
    };
    patch(&root, "16.17.1", &[("champion", &file("Ahri"))], true);
    patch(
        &root,
        "16.18.1",
        &[("champion", &file("Ahri (v2 fixture)"))],
        false,
    );
    let m = mirror(&root);

    let names = m.champion_names(&[103, 86, 999_999]).await;
    assert_eq!(names.len(), 2, "unknown ids are absent, not guessed");
    assert_eq!(names[&103], "Ahri");

    // The mirror moves: a real per-version cache re-reads, and back again.
    m.set_current("16.18.1");
    assert_eq!(m.champion_names(&[103]).await[&103], "Ahri (v2 fixture)");
    m.set_current("16.17.1");
    assert_eq!(m.champion_names(&[103]).await[&103], "Ahri");
}

#[tokio::test]
async fn no_mirror_means_no_names() {
    let base = tempfile::tempdir().unwrap();
    assert!(
        mirror(&base.path().join("none"))
            .champion_names(&[103])
            .await
            .is_empty()
    );
}

/// SITE-03: a match's game build → the Data Dragon version of its patch.
#[test]
fn a_game_build_maps_to_its_patchs_newest_data_dragon_version() {
    let versions: Vec<String> = ["16.20.1", "16.19.2", "16.19.1", "16.2.1", "lolpatch_7.20"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        ddragon_version_for("16.20.824.8524", &versions).as_deref(),
        Some("16.20.1")
    );
    assert_eq!(
        ddragon_version_for("16.19.821.7343", &versions).as_deref(),
        Some("16.19.2"),
        "a patch with two releases gets the newest"
    );
    assert_eq!(
        ddragon_version_for("16.2.700.1", &versions).as_deref(),
        Some("16.2.1"),
        "16.2 is not a prefix match for 16.20"
    );
    assert_eq!(
        ddragon_version_for("16.21.900.1", &versions),
        None,
        "not in the list yet"
    );
    for bad in ["", "16", "16.", ".19.1", "x.19.1", "16.x.1"] {
        assert_eq!(ddragon_version_for(bad, &versions), None, "{bad:?}");
    }
    assert_eq!(ddragon_version_for("16.20.824.8524", &[]), None);
}

#[tokio::test]
async fn versions_are_the_current_patchs_list_and_follow_a_new_patch() {
    let dir = tempfile::tempdir().unwrap();
    let m = mirror(dir.path());
    assert!(m.versions().await.is_empty(), "nothing mirrored yet");
    patch(
        dir.path(),
        "16.19.1",
        &[("versions", r#"["16.19.1","16.18.1"]"#)],
        false,
    );
    assert_eq!(*m.versions().await, ["16.19.1", "16.18.1"]);
    patch(
        dir.path(),
        "16.20.1",
        &[("versions", r#"["16.20.1","16.19.1"]"#)],
        false,
    );
    m.set_current("16.20.1");
    assert_eq!(
        *m.versions().await,
        ["16.20.1", "16.19.1"],
        "re-read for the new patch"
    );
}
