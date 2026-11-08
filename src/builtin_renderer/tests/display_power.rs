use super::*;

#[test]
fn display_off_blanks_the_frame_and_drops_contacts_until_power_returns() {
    let mut session = session();
    session.render().expect("render stock frame");
    let stock = session.frame.as_mut_slice().to_vec();
    session.dirty = false;

    let off = DesktopState {
        capabilities: DesktopCapabilities::DISPLAY_POWER,
        volume: None,
        muted: false,
        display_off: true,
    };
    session.set_desktop_state(off).expect("apply display off");
    assert!(session.display_off);
    assert!(session.dirty);
    session.render().expect("render dark frame");
    assert!(session.frame.as_mut_slice().iter().all(|byte| *byte == 0));

    let outcome = session
        .observe_input(raw_input(1, false, vec![contact(1, 5, 10)]))
        .expect("observe contact on dark panel");
    assert!(outcome.actions.is_empty());
    assert!(!outcome.cancel_touch_id);
    assert_eq!(session.input.pressed(), None);
    let outcome = session
        .observe_input(raw_input(2, false, Vec::new()))
        .expect("observe release on dark panel");
    assert!(outcome.actions.is_empty());

    let on = DesktopState {
        display_off: false,
        ..off
    };
    session.dirty = false;
    session.set_desktop_state(on).expect("apply display on");
    assert!(!session.display_off);
    assert!(session.dirty);
    session.render().expect("render restored frame");
    assert_eq!(session.frame.as_mut_slice(), stock);
    session
        .observe_input(raw_input(3, false, vec![contact(1, 5, 10)]))
        .expect("observe contact on lit panel");
    assert_eq!(session.input.pressed(), Some(StockAction::Escape));
}

#[test]
fn display_power_applies_under_an_active_touch_while_layout_waits() {
    let mut session = session();
    session
        .input
        .ingest(&input(1, false, vec![contact(1, 60, 10)]), None)
        .expect("start active touch");
    let state = DesktopState {
        capabilities: DesktopCapabilities::AUDIO | DesktopCapabilities::DISPLAY_POWER,
        volume: Some(50),
        muted: false,
        display_off: true,
    };
    session
        .set_desktop_state(state)
        .expect("defer layout but not display power");
    assert!(session.display_off);
    assert_eq!(session.desktop_state, DesktopState::default());
    assert_eq!(session.pending_desktop_state, Some(state));
    session.render().expect("render dark frame under touch");
    assert!(session.frame.as_mut_slice().iter().all(|byte| *byte == 0));
}

fn off_state() -> DesktopState {
    DesktopState {
        capabilities: DesktopCapabilities::DISPLAY_POWER,
        display_off: true,
        ..DesktopState::default()
    }
}

#[test]
fn blanking_cancels_held_escape_without_a_tap() {
    let mut session = session();
    session
        .observe_input(input(1, false, vec![contact(1, 5, 10)]))
        .unwrap();
    session.set_desktop_state(off_state()).unwrap();
    for (timestamp, contacts) in [(2, vec![contact(1, 5, 10)]), (3, Vec::new())] {
        let outcome = session
            .observe_input(raw_input(timestamp, true, contacts))
            .unwrap();
        assert!(
            outcome.actions.is_empty(),
            "dark panel dispatched {:?}",
            outcome.actions
        );
        assert!(!outcome.cancel_touch_id);
    }
}

#[test]
fn contacts_started_while_dark_wait_for_release_after_provider_failure() {
    let mut session = session();
    session.set_desktop_state(off_state()).unwrap();
    session.set_overlay(Some(OverlayState::Authenticate), None);
    let held = vec![contact(1, 24, 10), contact(2, 60, 10)];
    session
        .observe_input(raw_input(1, true, held.clone()))
        .unwrap();
    assert!(session.input.fn_pressed());
    session.set_desktop_state(DesktopState::default()).unwrap();
    for (timestamp, contacts) in [(2, held), (3, vec![contact(2, 60, 10)]), (4, Vec::new())] {
        let outcome = session
            .observe_input(raw_input(timestamp, true, contacts))
            .unwrap();
        assert!(outcome.actions.is_empty());
        assert!(!outcome.cancel_touch_id);
        assert_eq!(session.input.pressed(), None);
    }
    session.apply_pending_desktop_state().unwrap();
    let outcome = session
        .observe_input(raw_input(5, true, vec![contact(3, 60, 10)]))
        .unwrap();
    assert!(outcome.cancel_touch_id, "fresh press can cancel Touch ID");
}
