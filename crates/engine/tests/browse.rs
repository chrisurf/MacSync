//! Sync manager listing: state of every item on both Macs.

mod common;

use std::fs;

use common::Pair;
use macsync_engine::browse::{ItemState, Presence};
use macsync_engine::PeerApi;

fn state(items: &[macsync_engine::browse::BrowseItem], name: &str) -> (ItemState, Presence, Presence) {
    let i = items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("{name} missing in {items:#?}"));
    (i.state, i.local, i.remote)
}

#[test]
fn listing_shows_direction_per_item() {
    let p = Pair::new();
    fs::create_dir_all(p.ra().join("notes/sub")).unwrap();
    fs::write(p.ra().join("notes/same.txt"), "same").unwrap();
    fs::write(p.ra().join("notes/edit.txt"), "v1").unwrap();
    fs::write(p.ra().join("notes/gone.txt"), "x").unwrap();
    fs::write(p.ra().join("loose.txt"), "root file").unwrap();
    p.sync();

    // After a full sync everything is in sync on both sides.
    let root = p.a.browse(Some(&p.b as &dyn PeerApi), None, "").unwrap();
    assert_eq!(state(&root, "notes"), (ItemState::Synced, Presence::Present, Presence::Present));
    assert_eq!(state(&root, "loose.txt").0, ItemState::Synced);
    assert!(root.iter().find(|i| i.name == "notes").unwrap().is_unit);

    // Changes on A only (not yet pulled by B).
    fs::write(p.ra().join("notes/edit.txt"), "v2 from A").unwrap();
    fs::write(p.ra().join("notes/new.txt"), "new").unwrap();
    fs::remove_file(p.ra().join("notes/gone.txt")).unwrap();
    p.a.scan_all(false).unwrap();

    let root = p.a.browse(Some(&p.b as &dyn PeerApi), None, "").unwrap();
    assert_eq!(state(&root, "notes").0, ItemState::Differs);

    let a = p.a.browse(Some(&p.b as &dyn PeerApi), Some("notes"), "").unwrap();
    assert_eq!(state(&a, "same.txt").0, ItemState::Synced);
    assert_eq!(state(&a, "sub").0, ItemState::Synced);
    assert_eq!(state(&a, "edit.txt"), (ItemState::ToPeer, Presence::Present, Presence::Present));
    assert_eq!(state(&a, "new.txt"), (ItemState::ToPeer, Presence::Present, Presence::Missing));
    assert_eq!(state(&a, "gone.txt"), (ItemState::DeleteOnPeer, Presence::Deleted, Presence::Present));
    assert_eq!(a.iter().find(|i| i.name == "new.txt").unwrap().size, Some(3));

    // The same seen from B.
    let b = p.b.browse(Some(&p.a as &dyn PeerApi), Some("notes"), "").unwrap();
    assert_eq!(state(&b, "edit.txt").0, ItemState::FromPeer);
    assert_eq!(state(&b, "new.txt"), (ItemState::FromPeer, Presence::Missing, Presence::Present));
    assert_eq!(state(&b, "gone.txt"), (ItemState::DeleteHere, Presence::Present, Presence::Deleted));

    // Offline: local items only, state unknown.
    let off = p.a.browse(None, Some("notes"), "").unwrap();
    assert_eq!(state(&off, "same.txt"), (ItemState::Unknown, Presence::Present, Presence::Unknown));

    p.sync();
    let a = p.a.browse(Some(&p.b as &dyn PeerApi), Some("notes"), "").unwrap();
    assert!(a.iter().all(|i| i.state == ItemState::Synced), "{a:#?}");
    assert!(a.iter().all(|i| i.name != "gone.txt"));
}

#[test]
fn deleted_unit_disappears() {
    let p = Pair::new();
    fs::create_dir_all(p.ra().join("old")).unwrap();
    fs::write(p.ra().join("old/a.txt"), "a").unwrap();
    fs::create_dir_all(p.ra().join("keep")).unwrap();
    fs::write(p.ra().join("keep/b.txt"), "b").unwrap();
    p.sync();
    assert!(p.rb().join("old/a.txt").exists());

    // Deleted on A: B still has it until it pulls.
    fs::remove_dir_all(p.ra().join("old")).unwrap();
    p.a.scan_all(false).unwrap();
    let a = p.a.browse(Some(&p.b as &dyn PeerApi), None, "").unwrap();
    assert_eq!(state(&a, "old"), (ItemState::DeleteOnPeer, Presence::Deleted, Presence::Present));
    let b = p.b.browse(Some(&p.a as &dyn PeerApi), None, "").unwrap();
    assert_eq!(state(&b, "old"), (ItemState::DeleteHere, Presence::Present, Presence::Deleted));

    // After the sync the tombstones stay in both databases, but the unit is
    // gone from the listing on both sides.
    p.sync();
    assert!(!p.rb().join("old").exists());
    for (e, peer) in [(&p.a, &p.b), (&p.b, &p.a)] {
        let items = e.browse(Some(peer as &dyn PeerApi), None, "").unwrap();
        assert!(items.iter().all(|i| i.name != "old"), "{items:#?}");
        assert_eq!(state(&items, "keep").0, ItemState::Synced);
        let offline = e.browse(None, None, "").unwrap();
        assert!(offline.iter().all(|i| i.name != "old"));
    }
}
