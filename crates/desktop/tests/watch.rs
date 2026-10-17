//! Watch-folder imports.

mod support;

use std::time::Duration;

use shorts_desktop::watch::{WatchConfig, WatchFolder};
use shorts_desktop::{AppEvent, LibraryQuery};
use support::harness;

fn fast() -> WatchConfig {
    WatchConfig {
        settle: Duration::from_millis(300),
        poll: Duration::from_millis(50),
    }
}

async fn next_import(rx: &mut tokio::sync::broadcast::Receiver<AppEvent>) -> AppEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = rx.recv().await.unwrap();
            if matches!(
                event,
                AppEvent::Imported { .. } | AppEvent::ImportFailed { .. }
            ) {
                return event;
            }
        }
    })
    .await
    .expect("no import event")
}

#[tokio::test]
async fn new_files_are_imported_once_they_stop_growing() {
    let h = harness().await;
    let folder = h.dir.path().join("exports-from-tiktok");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("already-there.mp4"), b"existing clip").unwrap();
    let mut rx = h.app.subscribe();
    let watch = WatchFolder::start(h.app.clone(), folder.clone(), fast()).unwrap();
    assert_eq!(watch.dir(), folder);

    assert!(matches!(
        next_import(&mut rx).await,
        AppEvent::Imported {
            duplicate: false,
            ..
        }
    ));

    // A file written in two parts is imported once, after it settles.
    let path = folder.join("new clip.mp4");
    std::fs::write(&path, b"first half").unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(&path, b"first half and the second half").unwrap();
    std::fs::write(folder.join("readme.txt"), b"ignored").unwrap();
    assert!(matches!(
        next_import(&mut rx).await,
        AppEvent::Imported {
            duplicate: false,
            ..
        }
    ));

    let assets = h.app.list_assets(&LibraryQuery::default()).await.unwrap();
    let mut titles: Vec<_> = assets.iter().map(|a| a.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, vec!["already there", "new clip"]);
    let imported = &assets.iter().find(|a| a.title == "new clip").unwrap().media;
    assert_eq!(imported.as_ref().unwrap().size_bytes, 30);
    drop(watch);

    // Restarting doesn't re-import what was already handled.
    let mut rx = h.app.subscribe();
    let _watch = WatchFolder::start(h.app.clone(), folder.clone(), fast()).unwrap();
    std::fs::write(folder.join("third.mp4"), b"third clip").unwrap();
    match next_import(&mut rx).await {
        AppEvent::Imported { duplicate, .. } => assert!(!duplicate),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        h.app
            .list_assets(&LibraryQuery::default())
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn unreadable_files_are_reported() {
    let h = harness().await;
    let folder = h.dir.path().join("w");
    std::fs::create_dir_all(&folder).unwrap();
    let mut rx = h.app.subscribe();
    let _watch = WatchFolder::start(h.app.clone(), folder.clone(), fast()).unwrap();
    std::fs::write(folder.join("broken.mp4"), b"bad data").unwrap();
    match next_import(&mut rx).await {
        AppEvent::ImportFailed { path, .. } => assert!(path.ends_with("broken.mp4")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn watching_a_missing_folder_fails() {
    let h = harness().await;
    assert!(WatchFolder::start(h.app.clone(), h.dir.path().join("nope"), fast()).is_err());
}
