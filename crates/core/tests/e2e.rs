//! End-to-end tests: real endpoints and real transfers, all on loopback with
//! relays and discovery off, so they run anywhere `cargo test` does,
//! including machines where UDP to the LAN address is blocked.
//!
//! Each test drains the event channel into a `Vec<Event>` so it can assert on
//! what a UI would have seen, not just on the files.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use hither_core::{
    AcceptPolicy, CancellationToken, Event, EventSender, Identity, Inbox, InboxOptions, NetOptions,
    ReceiveOptions, SendOptions, Sender, TicketKind, inbox, link, receive, send_to,
};
use tokio::task::JoinHandle;

const TEST_TIMEOUT: Duration = Duration::from_secs(90);

// ---------------------------------------------------------------- helpers

/// A small tree: a nested folder, a multi-megabyte random file (several
/// chunk groups), a tiny file that the store keeps inline, and an empty one.
fn make_tree(root: &Path, big_bytes: usize) -> PathBuf {
    let album = root.join("album");
    std::fs::create_dir_all(album.join("roll-12")).unwrap();
    let mut big = vec![0u8; big_bytes];
    // Deterministic pseudo-random content: cheap and never compresses away.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for b in big.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    std::fs::write(album.join("roll-12").join("0001.tiff"), &big).unwrap();
    std::fs::write(album.join("notes.txt"), b"contact sheet, roll 12").unwrap();
    std::fs::write(album.join("empty.txt"), b"").unwrap();
    album
}

fn assert_same_tree(a: &Path, b: &Path) {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".hither-partial")
            {
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    walk(a, a, &mut left);
    walk(b, b, &mut right);
    left.sort();
    right.sort();
    assert_eq!(
        left.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        right.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        "different file lists"
    );
    for ((p, l), (_, r)) in left.iter().zip(&right) {
        assert!(l == r, "{} differs", p.display());
    }
}

/// An event sink that records everything, and a handle to get it back.
fn collector() -> (EventSender, JoinHandle<Vec<Event>>) {
    let (tx, mut rx) = hither_core::channel();
    let task = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(ev) = rx.recv().await {
            seen.push(ev);
        }
        seen
    });
    (tx, task)
}

fn has<F: Fn(&Event) -> bool>(events: &[Event], f: F) -> bool {
    events.iter().any(f)
}

fn local_send_options() -> SendOptions {
    SendOptions {
        net: NetOptions::local(),
        ticket_kind: TicketKind::Full,
        link_base: Some("https://example.test/".into()),
        ..SendOptions::default()
    }
}

async fn with_timeout<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test timed out")
}

// ------------------------------------------------------------------ tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn share_then_receive_roundtrip() {
    with_timeout(async {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let album = make_tree(src.path(), 3 * 1024 * 1024);

        let (stx, sevents) = collector();
        let sender = Sender::start(std::slice::from_ref(&album), local_send_options(), stx)
            .await
            .expect("sender starts");
        assert_eq!(sender.files().len(), 3);
        let link_str = sender.link().expect("link is printed").to_string();
        assert!(
            link_str.starts_with("https://example.test/#blob"),
            "{link_str}"
        );
        // The receiver gets what a person would paste: the link.
        let ticket = link::parse(&link_str).unwrap();

        let (rtx, revents) = collector();
        let received = receive(
            ticket,
            ReceiveOptions {
                out_dir: dst.path().to_path_buf(),
                net: NetOptions::local(),
            },
            rtx,
            CancellationToken::new(),
        )
        .await
        .expect("receive succeeds");

        assert_eq!(received.files.len(), 3);
        assert_same_tree(src.path(), dst.path());
        assert!(
            !hither_core::partial_dir(dst.path(), &sender.ticket().hash()).exists(),
            "partial store is removed on success"
        );

        sender.shutdown().await.unwrap();
        let sevents = sevents.await.unwrap();
        let revents = revents.await.unwrap();
        assert!(has(&sevents, |e| matches!(e, Event::Ready { .. })));
        assert!(has(&sevents, |e| matches!(e, Event::PeerConnected { .. })));
        assert!(has(
            &sevents,
            |e| matches!(e, Event::UploadDone { bytes, .. } if *bytes > 0)
        ));
        assert!(has(
            &revents,
            |e| matches!(e, Event::ManifestReceived { files, have: 0, .. } if files.len() == 3)
        ));
        assert!(has(
            &revents,
            |e| matches!(e, Event::DownloadDone { bytes, .. } if *bytes > 3 * 1024 * 1024)
        ));
        assert!(has(&revents, |e| matches!(
            e,
            Event::Finished { files: 3, .. }
        )));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receive_refuses_to_overwrite_and_leaves_nothing_behind() {
    with_timeout(async {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let album = make_tree(src.path(), 256 * 1024);
        // A file that would collide.
        std::fs::create_dir_all(dst.path().join("album")).unwrap();
        std::fs::write(dst.path().join("album/notes.txt"), b"mine").unwrap();

        let (stx, _) = collector();
        let sender = Sender::start(&[album], local_send_options(), stx)
            .await
            .unwrap();
        let (rtx, revents) = collector();
        let err = receive(
            sender.ticket().clone(),
            ReceiveOptions {
                out_dir: dst.path().to_path_buf(),
                net: NetOptions::local(),
            },
            rtx,
            CancellationToken::new(),
        )
        .await
        .expect_err("must refuse");
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        assert_eq!(
            std::fs::read(dst.path().join("album/notes.txt")).unwrap(),
            b"mine"
        );
        assert!(!hither_core::partial_dir(dst.path(), &sender.ticket().hash()).exists());
        sender.shutdown().await.unwrap();
        let revents = revents.await.unwrap();
        // It learned the file list but never started the payload.
        assert!(has(&revents, |e| matches!(
            e,
            Event::ManifestReceived { .. }
        )));
        assert!(!has(&revents, |e| matches!(e, Event::DownloadDone { .. })));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_then_resume_ends_identical() {
    with_timeout(async {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let album = make_tree(src.path(), 48 * 1024 * 1024);
        let (stx, _) = collector();
        let sender = Sender::start(&[album], local_send_options(), stx).await.unwrap();
        let ticket = sender.ticket().clone();
        let opts = ReceiveOptions {
            out_dir: dst.path().to_path_buf(),
            net: NetOptions::local(),
        };

        // First attempt: cancel at the first sign of payload moving.
        let (tx, mut rx) = hither_core::channel();
        let cancel = CancellationToken::new();
        let watcher = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut seen = Vec::new();
                while let Some(ev) = rx.recv().await {
                    if matches!(&ev, Event::DownloadProgress { bytes, total } if *bytes > 0 && bytes < total) {
                        cancel.cancel();
                    }
                    seen.push(ev);
                }
                seen
            })
        };
        let first = receive(ticket.clone(), opts.clone(), tx, cancel).await;
        let first_events = watcher.await.unwrap();

        match first {
            Err(e) => {
                assert!(e.downcast_ref::<hither_core::Cancelled>().is_some(), "{e:#}");
                assert!(hither_core::partial_dir(dst.path(), &ticket.hash()).exists(), "partial kept");
                // Second attempt resumes and finishes.
                let (tx, events) = collector();
                receive(ticket.clone(), opts, tx, CancellationToken::new())
                    .await
                    .expect("resume succeeds");
                let events = events.await.unwrap();
                assert!(has(&events, |e| matches!(e, Event::ManifestReceived { have, .. } if *have > 0)), "reports resumed bytes");
            }
            // Loopback was faster than the first progress event; that is a
            // plain roundtrip and still must be correct.
            Ok(_) => assert!(has(&first_events, |e| matches!(e, Event::Finished { .. }))),
        }
        assert_same_tree(src.path(), dst.path());
        sender.shutdown().await.unwrap();
    })
    .await;
}

// ------------------------------------------------------------------ inbox

async fn open_local_inbox(
    dir: &Path,
    policy: AcceptPolicy,
    events: EventSender,
) -> (Inbox, tempfile::TempDir) {
    let id_dir = tempfile::tempdir().unwrap();
    let identity = Identity::load_or_create(id_dir.path().join("identity")).unwrap();
    let inbox = Inbox::open(
        &identity,
        inbox::random_token(),
        InboxOptions {
            dir: dir.to_path_buf(),
            net: NetOptions::local(),
            policy,
            link_base: None,
        },
        events,
    )
    .await
    .expect("inbox opens");
    (inbox, id_dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbox_accepts_automatically_and_pulls() {
    with_timeout(async {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let album = make_tree(src.path(), 1024 * 1024);
        let (itx, ievents) = collector();
        let (inbox, _id) = open_local_inbox(dst.path(), AcceptPolicy::AcceptAll, itx).await;
        let ticket = inbox.ticket().clone();
        // The link form must round-trip too.
        let parsed = link::parse_inbox(&format!("https://example.test/#{ticket}")).unwrap();
        assert_eq!(parsed, ticket);

        let (ttx, tevents) = collector();
        let delivered = send_to(
            &ticket,
            &[album],
            Some("Sam".into()),
            local_send_options(),
            ttx,
            CancellationToken::new(),
        )
        .await
        .expect("delivery succeeds");
        assert_eq!(delivered.files, 3);

        inbox.shutdown().await.unwrap();
        let ievents = ievents.await.unwrap();
        let tevents = tevents.await.unwrap();
        assert!(has(
            &ievents,
            |e| matches!(e, Event::Offer { pending: false, label: Some(l), .. } if l == "Sam")
        ));
        assert!(has(&ievents, |e| matches!(
            e,
            Event::OfferDone { files: 3, .. }
        )));
        assert!(has(&tevents, |e| matches!(e, Event::ToAccepted)));
        assert!(has(&tevents, |e| matches!(
            e,
            Event::ToDone { files: 3, .. }
        )));

        // Landed in a folder named after the label.
        let folder = std::fs::read_dir(dst.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.is_dir() && p.file_name().unwrap().to_string_lossy().starts_with("sam-"))
            .expect("sam-<timestamp> folder");
        assert_same_tree(src.path(), &folder);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbox_asks_and_honours_the_answer() {
    with_timeout(async {
        for accept in [true, false] {
            let src = tempfile::tempdir().unwrap();
            let dst = tempfile::tempdir().unwrap();
            let album = make_tree(src.path(), 128 * 1024);
            let (itx, mut irx) = hither_core::channel();
            let (inbox, _id) = open_local_inbox(dst.path(), AcceptPolicy::Ask, itx).await;
            let decider = inbox.decider();
            // The "UI": answer the first pending offer.
            let answerer = tokio::spawn(async move {
                let mut seen = Vec::new();
                while let Some(ev) = irx.recv().await {
                    if let Event::Offer {
                        id, pending: true, ..
                    } = &ev
                    {
                        assert!(decider.decide(*id, accept));
                    }
                    seen.push(ev);
                }
                seen
            });

            let (ttx, _) = collector();
            let result = send_to(
                inbox.ticket(),
                &[album],
                None,
                local_send_options(),
                ttx,
                CancellationToken::new(),
            )
            .await;
            inbox.shutdown().await.unwrap();
            let ievents = answerer.await.unwrap();
            if accept {
                result.expect("accepted offer is delivered");
                assert!(has(&ievents, |e| matches!(e, Event::OfferDone { .. })));
                assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 1);
            } else {
                let err = result.expect_err("declined offer fails on the sender side");
                assert!(format!("{err:#}").contains("declined"), "{err:#}");
                assert!(has(&ievents, |e| matches!(e, Event::OfferDeclined { .. })));
                assert_eq!(
                    std::fs::read_dir(dst.path()).unwrap().count(),
                    0,
                    "nothing written"
                );
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbox_rejects_a_wrong_token_without_an_event() {
    with_timeout(async {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let album = make_tree(src.path(), 64 * 1024);
        let (itx, ievents) = collector();
        let (inbox, _id) = open_local_inbox(dst.path(), AcceptPolicy::AcceptAll, itx).await;
        let mut forged = inbox.ticket().clone();
        forged.token = [0u8; 16];
        let (ttx, _) = collector();
        let err = send_to(
            &forged,
            &[album],
            None,
            local_send_options(),
            ttx,
            CancellationToken::new(),
        )
        .await
        .expect_err("wrong token is refused");
        assert!(format!("{err:#}").contains("not valid"), "{err:#}");
        inbox.shutdown().await.unwrap();
        let ievents = ievents.await.unwrap();
        assert!(
            !has(&ievents, |e| matches!(e, Event::Offer { .. })),
            "a probe must not surface as an offer"
        );
    })
    .await;
}

// ------------------------------------------------------------------- codes

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spoken_code_hands_over_the_ticket() {
    with_timeout(async {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let album = make_tree(src.path(), 256 * 1024);
        let (stx, _) = collector();
        let sender = Sender::start(
            &[album],
            SendOptions {
                code: true,
                ..local_send_options()
            },
            stx,
        )
        .await
        .unwrap();
        let code = sender.code().expect("a code was minted").clone();
        let spoken = code.to_string();
        assert_eq!(spoken.split('-').count(), 4, "{spoken}");

        // The receiver knows only the words. In tests it also learns the
        // meeting point's address, standing in for DNS discovery.
        let parsed = hither_core::Code::parse(&spoken.replace('-', " ")).unwrap();
        let net = NetOptions::local().knowing(sender.code_endpoint().unwrap().addr());
        let payload = hither_core::code::redeem(&parsed, &net)
            .await
            .expect("code resolves");
        let ticket = link::parse(&payload).expect("payload is the share ticket");
        assert_eq!(ticket.hash(), sender.ticket().hash());

        let (rtx, _) = collector();
        receive(
            ticket,
            ReceiveOptions {
                out_dir: dst.path().to_path_buf(),
                net: NetOptions::local(),
            },
            rtx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_same_tree(src.path(), dst.path());

        // A wrong code is a different endpoint that nobody runs.
        let wrong = hither_core::Code::parse("abandon abandon abandon abandon").unwrap();
        assert_ne!(wrong.endpoint_id(), parsed.endpoint_id());
        sender.shutdown().await.unwrap();
    })
    .await;
}
