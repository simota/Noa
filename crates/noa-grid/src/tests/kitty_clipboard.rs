// ── Kitty clipboard protocol (OSC 5522) ─────────────────────────────

use crate::kitty_clipboard::{
    ClipboardLocation, KittyClipboardRequest, KittyClipboardStatus, paste_event,
};

fn kc_b64(data: &str) -> String {
    let mut out = Vec::new();
    crate::osc::encode_base64(data.as_bytes(), &mut out);
    String::from_utf8(out).unwrap()
}

/// `ESC ] 5522 ; <body> ESC \`.
fn kc_osc(body: &str) -> Vec<u8> {
    format!("\x1b]5522;{body}\x1b\\").into_bytes()
}

fn kc_feed(t: &mut Terminal, body: &str) {
    Stream::new().feed(&kc_osc(body), t);
}

fn kc_requests(t: &mut Terminal) -> Vec<KittyClipboardRequest> {
    t.take_pending_kitty_clipboard()
}

fn kc_read(t: &mut Terminal) -> crate::kitty_clipboard::KittyClipboardRead {
    match kc_requests(t).as_slice() {
        [KittyClipboardRequest::Read(read)] => read.clone(),
        other => panic!("expected one read, got {other:?}"),
    }
}

fn kc_write(t: &mut Terminal) -> crate::kitty_clipboard::KittyClipboardWrite {
    match kc_requests(t).as_slice() {
        [KittyClipboardRequest::Write(write)] => write.clone(),
        other => panic!("expected one write, got {other:?}"),
    }
}

fn kc_contents(write: &crate::kitty_clipboard::KittyClipboardWrite) -> Vec<(&str, &[u8])> {
    write
        .contents
        .iter()
        .map(|(mime, data)| (mime.as_str(), &**data))
        .collect()
}

fn kc_terminal() -> Terminal {
    Terminal::new(GridSize::new(80, 24))
}

#[test]
fn kitty_clipboard_read_request_and_success_reply() {
    let mut t = kc_terminal();
    Stream::new().feed(
        format!("\x1b]5522;type=read:id=a/b;{}\x07", kc_b64("text/plain .")).as_bytes(),
        &mut t,
    );
    let read = kc_read(&mut t);
    assert_eq!(read.mimes, vec!["text/plain".to_string()]);
    assert!(read.list);
    assert!(!read.granted);
    assert!(t.pending_writes.is_empty());

    let reply = read.reply_success(
        &["text/plain".into(), "image/png".into()],
        &[Some(b"hi".to_vec())],
    );
    let expected = format!(
        "\x1b]5522;type=read:status=OK:id=ab\x07\
         \x1b]5522;type=read:status=DATA:id=ab:mime=Lg==;{}\x07\
         \x1b]5522;type=read:status=DATA:id=ab:mime={};aGk=\x07\
         \x1b]5522;type=read:status=DONE:id=ab\x07",
        kc_b64("text/plain image/png\n"),
        kc_b64("text/plain"),
    );
    assert_eq!(String::from_utf8(reply).unwrap(), expected);
    assert_eq!(
        read.reply_status(KittyClipboardStatus::Eperm),
        b"\x1b]5522;type=read:status=EPERM:id=ab\x07"
    );
}

#[test]
fn kitty_clipboard_read_data_is_chunked_and_loc_primary_only_on_ok() {
    let mut t = kc_terminal();
    kc_feed(&mut t, &format!("type=read:loc=primary;{}", kc_b64("a/b")));
    let read = kc_read(&mut t);
    assert_eq!(read.location, ClipboardLocation::Primary);
    let reply = String::from_utf8(read.reply_success(&[], &[Some(vec![b'x'; 4097])])).unwrap();
    assert!(reply.starts_with("\x1b]5522;type=read:status=OK:loc=primary\x1b\\"));
    assert_eq!(reply.matches("status=DATA").count(), 2, "4096 + 1 bytes");
    assert_eq!(reply.matches("loc=primary").count(), 1);
    // An empty listing packet is still sent when asked for; empty data is not.
    let mut t = kc_terminal();
    kc_feed(&mut t, &format!("type=read;{}", kc_b64(". a/b")));
    let reply = String::from_utf8(kc_read(&mut t).reply_success(&[], &[Some(Vec::new())])).unwrap();
    assert_eq!(
        reply,
        "\x1b]5522;type=read:status=OK\x1b\\\
         \x1b]5522;type=read:status=DATA:mime=Lg==;\x1b\\\
         \x1b]5522;type=read:status=DONE\x1b\\"
    );
}

#[test]
fn kitty_clipboard_read_keeps_four_data_types() {
    let mut t = kc_terminal();
    kc_feed(&mut t, &format!("type=read;{}", kc_b64("a b . c d e")));
    let read = kc_read(&mut t);
    assert_eq!(read.mimes, ["a", "b", "c", "d"]);
    assert!(read.list);
}

#[test]
fn kitty_clipboard_malformed_packets_are_dropped_silently() {
    for body in [
        "",
        "type=read:loc",
        "type=bogus",
        "loc=primary",
        // Unpadded, whitespace, and non-UTF-8 payloads.
        "type=read;dGV4dA",
        "type=read;dGV4 dA==",
        "type=read;/w==",
        // A bad metadata value on a read.
        "type=read:mime=@@@@;dGV4dA==",
    ] {
        let mut t = kc_terminal();
        kc_feed(&mut t, body);
        assert!(kc_requests(&mut t).is_empty(), "{body:?}");
        assert!(t.pending_writes.is_empty(), "{body:?}");
    }
    // Unknown keys are ignored and the last duplicate wins.
    let mut t = kc_terminal();
    kc_feed(&mut t, "type=write:x=1:type=read");
    assert_eq!(kc_requests(&mut t).len(), 1);
}

#[test]
fn kitty_clipboard_write_streams_base64_across_packets() {
    let mut t = kc_terminal();
    let mime = kc_b64("text/plain");
    kc_feed(&mut t, "type=write:id=w1");
    // "Ghostty" = R2hvc3R0eQ==, split mid-group.
    kc_feed(&mut t, &format!("type=wdata:mime={mime};R2h"));
    kc_feed(&mut t, &format!("type=wdata:mime={mime};vc3R0eQ=="));
    // A packet ending on padding restarts the stream.
    kc_feed(&mut t, &format!("type=wdata:mime={mime};IQ=="));
    kc_feed(&mut t, "type=wdata");
    let write = kc_write(&mut t);
    assert_eq!(kc_contents(&write), [("text/plain", &b"Ghostty!"[..])]);
    assert_eq!(write.location, ClipboardLocation::Standard);
    assert!(!write.granted);
    assert_eq!(
        write.reply_status(KittyClipboardStatus::Done),
        b"\x1b]5522;type=write:status=DONE:id=w1\x1b\\"
    );
}

#[test]
fn kitty_clipboard_invalid_write_data_aborts_with_einval() {
    let mime = kc_b64("text/plain");
    for data in ["R2g=R2g=", "R2*v", "R2g"] {
        let mut t = kc_terminal();
        kc_feed(&mut t, "type=write:id=w1");
        kc_feed(&mut t, &format!("type=wdata:mime={mime};{data}"));
        kc_feed(&mut t, "type=wdata");
        assert_eq!(
            t.take_pending_writes(),
            b"\x1b]5522;type=write:status=EINVAL:id=w1\x1b\\",
            "{data:?}"
        );
        assert!(kc_requests(&mut t).is_empty(), "{data:?}");
    }
}

#[test]
fn kitty_clipboard_write_limit_is_inclusive_and_fixed_at_write() {
    let mime = kc_b64("a/b");
    let mut t = kc_terminal();
    t.kitty_clipboard_write_limit = Some(3);
    kc_feed(&mut t, "type=write");
    t.kitty_clipboard_write_limit = Some(0);
    kc_feed(&mut t, &format!("type=wdata:mime={mime};YWJj"));
    kc_feed(&mut t, "type=wdata");
    assert_eq!(&*kc_write(&mut t).contents[0].1, b"abc");

    t.kitty_clipboard_write_limit = Some(2);
    kc_feed(&mut t, "type=write:id=big");
    kc_feed(&mut t, &format!("type=wdata:mime={mime};YWJj"));
    assert_eq!(
        t.take_pending_writes(),
        b"\x1b]5522;type=write:status=EFBIG:id=big\x1b\\"
    );
    // Later packets of the aborted write are ignored.
    kc_feed(&mut t, "type=wdata");
    assert!(kc_requests(&mut t).is_empty());
    assert!(t.take_pending_writes().is_empty());
}

#[test]
fn kitty_clipboard_denied_write_replies_eperm_and_opens_nothing() {
    let mut t = kc_terminal();
    t.osc52_policy.allow_write = false;
    Stream::new().feed(b"\x1b]5522;type=write:id=x\x07", &mut t);
    assert_eq!(
        t.take_pending_writes(),
        b"\x1b]5522;type=write:status=EPERM:id=x\x07"
    );
    kc_feed(&mut t, "type=wdata");
    assert!(kc_requests(&mut t).is_empty());
}

#[test]
fn kitty_clipboard_aliases_resolve_in_order() {
    let mut t = kc_terminal();
    kc_feed(&mut t, "type=write");
    kc_feed(&mut t, &format!("type=wdata:mime={};YQ==", kc_b64("x/a")));
    kc_feed(&mut t, &format!("type=wdata:mime={};Yg==", kc_b64("x/b")));
    // x/c → x/a, then x/d → x/c (a chain), x/b → x/a (overwrites x/b's
    // data), and x/e → a type with no data (dropped).
    kc_feed(&mut t, &format!("type=walias:mime={};{}", kc_b64("x/a"), kc_b64("x/c x/b")));
    kc_feed(&mut t, &format!("type=walias:mime={};{}", kc_b64("x/c"), kc_b64("x/d")));
    kc_feed(&mut t, &format!("type=walias:mime={};{}", kc_b64("x/none"), kc_b64("x/e")));
    kc_feed(&mut t, "type=wdata");
    let write = kc_write(&mut t);
    let got = kc_contents(&write);
    assert_eq!(
        got,
        [("x/a", &b"a"[..]), ("x/b", b"a"), ("x/c", b"a"), ("x/d", b"a")]
    );
    // Aliases share their target's data rather than copying it.
    assert!(std::sync::Arc::ptr_eq(&write.contents[0].1, &write.contents[3].1));
}

#[test]
fn kitty_clipboard_resending_a_type_replaces_its_data() {
    let mut t = kc_terminal();
    let (a, b) = (kc_b64("x/a"), kc_b64("x/b"));
    kc_feed(&mut t, "type=write");
    kc_feed(&mut t, &format!("type=wdata:mime={a};YQ=="));
    kc_feed(&mut t, &format!("type=wdata:mime={b};Yg=="));
    kc_feed(&mut t, &format!("type=wdata:mime={a};Yw=="));
    kc_feed(&mut t, "type=wdata");
    let write = kc_write(&mut t);
    assert_eq!(kc_contents(&write), [("x/a", &b"c"[..]), ("x/b", b"b")]);
}

#[test]
fn kitty_clipboard_invalid_values_abort_only_data_packets() {
    // A bad value on wdata or walias aborts the open write with EINVAL…
    for body in ["type=wdata:mime=@@@@", "type=walias:name=@@@@"] {
        let mut t = kc_terminal();
        kc_feed(&mut t, "type=write:id=w");
        kc_feed(&mut t, body);
        assert_eq!(
            t.take_pending_writes(),
            b"\x1b]5522;type=write:status=EINVAL:id=w\x1b\\",
            "{body:?}"
        );
    }
    // …a walias with no target too…
    let mut t = kc_terminal();
    kc_feed(&mut t, "type=write:id=w");
    kc_feed(&mut t, &format!("type=walias;{}", kc_b64("x/a")));
    assert_eq!(
        t.take_pending_writes(),
        b"\x1b]5522;type=write:status=EINVAL:id=w\x1b\\"
    );
    // …but a bad `type=write` is dropped and leaves the open write running.
    let mut t = kc_terminal();
    kc_feed(&mut t, "type=write:id=w");
    kc_feed(&mut t, "type=write:id=v:mime=@@@@");
    kc_feed(&mut t, "type=wdata");
    assert!(t.take_pending_writes().is_empty());
    assert_eq!(
        kc_write(&mut t).reply_status(KittyClipboardStatus::Done),
        b"\x1b]5522;type=write:status=DONE:id=w\x1b\\"
    );
}

#[test]
fn kitty_clipboard_paste_event_grant_is_one_time_and_needs_a_name() {
    let mut t = kc_terminal();
    t.grant_kitty_clipboard("secret", true, false, true);
    let pw = kc_b64("secret");
    let name = kc_b64("prog");
    let data = kc_b64("text/plain");
    // A listing-only read and a read without a name don't use the grant.
    kc_feed(&mut t, &format!("type=read:pw={pw}:name={name};{}", kc_b64(".")));
    assert!(!kc_read(&mut t).granted);
    kc_feed(&mut t, &format!("type=read:pw={pw};{data}"));
    let read = kc_read(&mut t);
    assert!(!read.granted);
    assert_eq!(read.pw, "");
    // The first named data read uses it up.
    kc_feed(&mut t, &format!("type=read:pw={pw}:name={name};{data}"));
    assert!(kc_read(&mut t).granted);
    kc_feed(&mut t, &format!("type=read:pw={pw}:name={name};{data}"));
    assert!(!kc_read(&mut t).granted);
}

#[test]
fn kitty_clipboard_persistent_write_grant_covers_commits() {
    let mut t = kc_terminal();
    t.grant_kitty_clipboard("pw1", false, true, false);
    let header = format!("type=write:pw={}:name={}", kc_b64("pw1"), kc_b64("prog"));
    for _ in 0..2 {
        kc_feed(&mut t, &header);
        kc_feed(&mut t, "type=wdata");
        let write = kc_write(&mut t);
        assert!(write.granted);
        assert!(write.contents.is_empty());
    }
    // A read under a write-only grant is not covered.
    kc_feed(
        &mut t,
        &format!("type=read:pw={}:name={};{}", kc_b64("pw1"), kc_b64("prog"), kc_b64("a/b")),
    );
    assert!(!kc_read(&mut t).granted);
}

#[test]
fn kitty_paste_event_mode_is_reported_and_reset_by_ris() {
    let mut t = kc_terminal();
    let mut s = Stream::new();
    s.feed(b"\x1b[?5522h\x1b[?5522$p", &mut t);
    assert!(t.modes.kitty_paste_events());
    assert_eq!(t.take_pending_writes(), b"\x1b[?5522;1$y");
    t.grant_kitty_clipboard("pw", true, false, false);
    s.feed(b"\x1bc", &mut t);
    assert!(!t.modes.kitty_paste_events());
    kc_feed(
        &mut t,
        &format!("type=read:pw={}:name={};{}", kc_b64("pw"), kc_b64("p"), kc_b64("a/b")),
    );
    assert!(!kc_read(&mut t).granted, "RIS forgets grants");
}

#[test]
fn kitty_paste_event_bytes_match_ghostty() {
    let event = paste_event(
        "abc",
        ClipboardLocation::Primary,
        &["text/plain".into(), "image/png".into()],
    );
    let pw = kc_b64("abc");
    assert_eq!(
        String::from_utf8(event).unwrap(),
        format!(
            "\x1b]5522;type=read:status=OK:loc=primary:pw={pw}\x1b\\\
             \x1b]5522;type=read:status=DATA:mime=Lg==:pw={pw};{}\x1b\\\
             \x1b]5522;type=read:status=DONE:pw={pw}\x1b\\",
            kc_b64("text/plain image/png\n")
        )
    );
}
