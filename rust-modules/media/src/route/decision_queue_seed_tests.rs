//! Playing from a SHUFFLED PlayQueue: the landing a Shuffle press resolves to, and the queue the
//! publication shares with the Play queue page. Pure halves only — the POST is the plex crate's
//! (`timeline.rs` grades its wire shape).
use super::*;
use plx_plex::plex::QueueRow;

const S: ServerId = ServerId::from_raw(0);

fn ep(item_id: i64, rk: &str, season: i64, index: i64) -> QueueRow {
    QueueRow {
        item_id,
        sid: S,
        rk: rk.into(),
        kind: "episode".into(),
        title: format!("t{rk}"),
        show_title: "show".into(),
        season,
        index,
        part: format!("/library/parts/{rk}/file.mkv"),
        ..Default::default()
    }
}

/// A shuffled queue as the server answers it: episodes out of order, the second row selected.
fn shuffled() -> Vec<QueueRow> {
    vec![ep(11, "305", 3, 5), ep(12, "102", 1, 2), ep(13, "207", 2, 7), ep(14, "101", 1, 1)]
}

/// **A shuffle starts on the server's pick and keeps its queue.** The landing's first row is the
/// selected one (not the queue's first row), with the POST's rows already in hand (no second round
/// trip), so the continuation the first playback starts in is the shuffle itself.
#[test]
fn a_shuffle_lands_on_the_servers_pick_with_its_rows() {
    let landing = shuffle_landing_of(S, 41, 12, shuffled()).expect("a shuffle with rows lands");
    assert_eq!(landing.first.rk, "102");
    assert_eq!(landing.first.item_id, 12);
    assert_eq!(landing.queue_id, 41);
    assert_eq!(landing.rows.len(), 4);

    let unnamed = shuffle_landing_of(S, 41, 0, shuffled()).unwrap();
    assert_eq!(unnamed.first.rk, "305", "no selection named: the queue's head");
    assert!(shuffle_landing_of(S, 41, 12, Vec::new()).is_none(), "an empty shuffle starts nothing");
    assert!(shuffle_landing_of(S, 0, 12, shuffled()).is_none(), "a queue with no id cannot be continued");
}

/// **The shuffle's successor is the QUEUE's next row**, not the next episode in order: the
/// continuation built from the landing names S2E7 after S1E2.
#[test]
fn a_shuffle_continuation_names_the_shuffled_successor() {
    let landing = shuffle_landing_of(S, 41, 12, shuffled()).unwrap();
    let queue = super::super::queue_navigation::QueueContinuation::from_queue(
        landing.queue_id.to_string(), landing.rows, &landing.first,
    )
    .expect("the selected row is in its own queue");
    assert!(queue.matches(S, "102"));
    let q = queue.resolve();
    assert_eq!((q.id.as_str(), q.item_id.as_str()), ("41", "12"));
    let next = q.up_next.expect("a successor");
    assert_eq!((next.rk.as_str(), next.season, next.index), ("207", 2, 7));
    assert!(q.machine_id.is_empty(), "a queue that exists needs no /identity");
}

/// **The queue reaches the screens' copy, shared.** The Play queue page reads the rows off the
/// per-frame publication, so it must carry them — by reference, not as a copy of every row.
#[test]
fn the_publication_shares_the_queue() {
    let mut ps = PlaybackSession::IDLE;
    ps.queue = Some(std::sync::Arc::new(shuffled()));
    let a = ps.publication();
    with_queue(&a, |rows| assert_eq!(rows.len(), 4));
    with_queue(&ps, |held| with_queue(&a, |shown| assert_eq!(held.as_ptr(), shown.as_ptr())));
    with_queue(&PlaybackSession::IDLE, |rows| assert!(rows.is_empty()));
}
