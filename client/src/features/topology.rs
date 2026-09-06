//! "Where does my data actually go?", answered from this client's seat.
//!
//! Deliberately not a diagram of the system — a diagram of *this session*. The
//! honest answers differ per row (a self-host keeps the database on this disk;
//! a relayed connection hides your address from the host but not from the
//! relay) and a picture that averaged them would be wrong for everybody.
//!
//! Every claim here is one the code can back. Nothing aspirational: if a leg is
//! only encrypted in transit, it says so rather than showing a padlock.

use dioxus::prelude::*;

use crate::state::{Transport, use_app_state};

/// How protected a leg is, in the only three grades worth distinguishing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seal {
    /// Nobody in the middle can read it, including whoever forwards it.
    EndToEnd,
    /// Encrypted on the wire, but an endpoint on the path can read it.
    InTransit,
    /// Not encrypted, because it never leaves this machine.
    Local,
}

impl Seal {
    fn chip(self) -> (&'static str, &'static str) {
        match self {
            Seal::EndToEnd => ("end to end", "var(--success)"),
            Seal::InTransit => ("in transit", "var(--warn)"),
            Seal::Local => ("stays local", "var(--text-muted)"),
        }
    }
}

struct Leg {
    what: &'static str,
    /// Short enough for the box in the picture; `where_to` carries the address.
    node: &'static str,
    where_to: String,
    seal: Seal,
    note: String,
    /// Draws the little cylinder beside this node: the one that holds messages.
    holds_db: bool,
    /// `false` for a row that describes something staying put. It still earns a
    /// card below, but drawing an arrow to it would contradict what it says.
    travels: bool,
    /// A machine the traffic passes *through* on the way. Drawn on the arrow,
    /// because a hop that only appears in prose is a hop nobody reads about.
    via: Option<String>,
}

/// `<` and `&` in a hostname would otherwise close the tag they land in. The
/// SVG is built as a string because that is how every other picture in this
/// tree is built, and a string is the one place escaping is not automatic.
fn esc(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The box is 202px wide; past this an address reads as a smear, so the middle
/// goes rather than the end — the port is the half worth keeping.
fn ellipsize(raw: &str, max: usize) -> String {
    let count = raw.chars().count();
    if count <= max {
        return raw.to_string();
    }
    let keep = max.saturating_sub(1) / 2;
    let head: String = raw.chars().take(keep).collect();
    let tail: String = raw.chars().skip(count - keep).collect();
    format!("{head}…{tail}")
}

/// One box per destination, one arrow per leg, drawn from this client outward.
///
/// Dashed *and* coloured for a leg an endpoint can read: colour alone says
/// nothing to a reader who cannot separate the two, and that distinction is
/// the whole reason this panel exists.
///
/// What travels is the *heading* of each box and the destination is the line
/// under it, so nothing has to be written along a curve — a label on a bezier
/// crosses it at some window width, whatever the maths says at this one.
fn diagram_svg(legs: &[Leg], you: &str) -> String {
    const BOX_H: f64 = 50.0;
    const GAP: f64 = 14.0;
    const LEFT_W: f64 = 104.0;
    const RIGHT_X: f64 = 214.0;
    const RIGHT_W: f64 = 238.0;
    const PAD: f64 = 12.0;

    let travelling: Vec<&Leg> = legs.iter().filter(|l| l.travels).collect();
    let stays = legs.iter().filter(|l| !l.travels).count();

    let n = travelling.len() as f64;
    let height = n * BOX_H + (n - 1.0) * GAP + PAD * 2.0;
    let mid = height / 2.0;
    let x0 = 8.0 + LEFT_W;

    let mut out = format!(
        r##"<svg viewBox="0 0 460 {height}" width="100%" xmlns="http://www.w3.org/2000/svg">
<defs>
<marker id="dxa-e2e" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="var(--success)"/></marker>
<marker id="dxa-tr" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="var(--warn)"/></marker>
<marker id="dxa-lo" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="var(--text-muted)"/></marker>
</defs>
<rect x="8" y="{you_y}" width="{LEFT_W}" height="{BOX_H}" rx="9" fill="var(--accent-soft)" stroke="var(--accent)"/>
<text x="{you_cx}" y="{you_t1}" text-anchor="middle" font-size="12" font-weight="700" fill="var(--accent)">You</text>
<text x="{you_cx}" y="{you_t2}" text-anchor="middle" font-size="9" fill="var(--text-muted)">{you_label}</text>
"##,
        you_y = mid - BOX_H / 2.0,
        you_cx = 8.0 + LEFT_W / 2.0,
        you_t1 = mid - 1.0,
        you_t2 = mid + 12.0,
        you_label = esc(you),
    );

    // Said under the box it belongs to rather than drawn as an arrow: an arrow
    // to a box labelled "never leaves" is a picture arguing with its caption.
    if stays > 0 {
        out.push_str(&format!(
            r##"<text x="{cx}" y="{y}" text-anchor="middle" font-size="8.5" fill="var(--text-dim)">your key never leaves</text>
"##,
            cx = 8.0 + LEFT_W / 2.0,
            y = mid + BOX_H / 2.0 + 13.0,
        ));
    }

    for (i, leg) in travelling.iter().enumerate() {
        let top = PAD + i as f64 * (BOX_H + GAP);
        let cy = top + BOX_H / 2.0;
        let (stroke, marker, dash) = match leg.seal {
            Seal::EndToEnd => ("var(--success)", "dxa-e2e", ""),
            Seal::InTransit => ("var(--warn)", "dxa-tr", r#" stroke-dasharray="6 4""#),
            Seal::Local => ("var(--text-muted)", "dxa-lo", r#" stroke-dasharray="2 3""#),
        };
        out.push_str(&format!(
            r##"<path d="M{x0},{mid} C{c1},{mid} {c2},{cy} {arrow_end},{cy}" fill="none" stroke="{stroke}" stroke-width="1.8"{dash} marker-end="url(#{marker})"/>
<rect x="{RIGHT_X}" y="{top}" width="{RIGHT_W}" height="{BOX_H}" rx="9" fill="var(--panel2)" stroke="{stroke}" stroke-opacity="0.5"/>
<text x="{tx}" y="{t1}" text-anchor="middle" font-size="11" font-weight="600" fill="var(--text)">{what}</text>
<text x="{tx}" y="{t2}" text-anchor="middle" font-size="9" fill="var(--text-muted)">{node} · {addr}</text>
"##,
            c1 = x0 + 46.0,
            c2 = RIGHT_X - 46.0,
            arrow_end = RIGHT_X - 4.0,
            tx = RIGHT_X + RIGHT_W / 2.0,
            t1 = cy - 2.0,
            t2 = cy + 12.0,
            what = esc(leg.what),
            node = esc(leg.node),
            addr = esc(&ellipsize(&leg.where_to, 28)),
        ));
        if let Some(via) = &leg.via {
            // t = 0.5 on the cubic, which for these control points is the
            // visual middle closely enough to hang a label on.
            let vx = (x0 + RIGHT_X) / 2.0;
            let vy = (mid + cy) / 2.0;
            let label = ellipsize(via, 18);
            // Clamped, not merely sized: the gap between the two columns is
            // fixed, so a long hop name would otherwise sit on top of the box
            // it is supposed to be pointing at.
            let w = (8.0 + label.chars().count() as f64 * 4.6).min(RIGHT_X - x0 - 14.0);
            out.push_str(&format!(
                r##"<rect x="{rx}" y="{ry}" width="{w}" height="15" rx="7.5" fill="var(--panel-solid)" stroke="{stroke}" stroke-opacity="0.55"/>
<text x="{vx}" y="{ty}" text-anchor="middle" font-size="8" fill="var(--text-muted)">{label}</text>
"##,
                rx = vx - w / 2.0,
                ry = vy - 7.5,
                ty = vy + 3.0,
                label = esc(&label),
            ));
        }
        if leg.holds_db {
            // A cylinder, because that is what a database looks like to
            // everyone who has ever seen one drawn.
            let dx = RIGHT_X + RIGHT_W - 24.0;
            let dy = top + 11.0;
            out.push_str(&format!(
                r##"<g fill="none" stroke="var(--text-dim)" stroke-width="1.2"><ellipse cx="{cx}" cy="{dy}" rx="7" ry="2.6"/><path d="M{dx},{dy} v11 a7,2.6 0 0 0 14,0 v-11"/></g>
"##,
                cx = dx + 7.0,
            ));
        }
    }
    out.push_str("</svg>");
    out
}
#[component]
pub fn TopologyDialog(on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    let host = snapshot.host_info.clone();
    let transport = snapshot.transport;
    let origin = snapshot.server_origin.clone().unwrap_or_default();
    let rendezvous = snapshot.rendezvous_url.clone();
    let relays: Vec<String> = snapshot.nostr_relays_up.iter().cloned().collect();
    let in_voice = snapshot.voice.channel_id.is_some();
    let voice_sealed = snapshot
        .voice
        .channel_id
        .is_some_and(|c| snapshot.media_keys.contains_key(&c));
    drop(snapshot);

    let self_hosting = host.is_some();
    let voice_bundled = host.as_ref().map(|h| h.voice_bundled).unwrap_or(false);
    let rendezvous_label = rendezvous
        .as_deref()
        .and_then(|r| crate::protocol::host_origin(r, 443))
        .or_else(|| rendezvous.clone());

    // Only when it is actually carrying the connection. A rendezvous that
    // merely resolved a code is not on the path afterwards, and drawing it
    // there would be the same lie as leaving it out when it is.
    // The pill says only *that* there is a hop. Squeezing a hostname into it
    // truncated the hostname to nothing, and the row below names the machine
    // in full anyway.
    let relay_hop = matches!(transport, Transport::QuicRelayed).then(|| "via relay".to_string());

    let db_line = if self_hosting {
        "On this disk. You are the server, so every message, member and guild row is a file here and nowhere else."
    } else {
        "On the server's disk, not yours. Messages live only in its database (this client holds what it fetched, in memory)."
    };

    let (gateway_where, gateway_seal, gateway_note) = match (self_hosting, transport) {
        (true, _) => (
            "loopback on this machine".to_string(),
            Seal::Local,
            "The gateway is a thread in this process. This leg never reaches a network interface.".to_string(),
        ),
        (false, Transport::Quic) => (
            origin.clone(),
            Seal::EndToEnd,
            "QUIC straight to the host, authenticated by its key. Nobody in between can read it. The host sees your IP address.".to_string(),
        ),
        (false, Transport::QuicRelayed) => (
            origin.clone(),
            Seal::EndToEnd,
            "Carried by the rendezvous relay. It forwards ciphertext and sees only the two keys — but it, not the host, learns your address.".to_string(),
        ),
        (false, Transport::Proxied) => (
            origin.clone(),
            Seal::InTransit,
            "TLS to a proxy in front of the gateway. The proxy's operator — usually the server's — can read this leg. Nobody else on the path can.".to_string(),
        ),
        (false, Transport::Loopback) => (
            origin.clone(),
            Seal::Local,
            "A plain WebSocket to this machine. Allowed only because it is loopback.".to_string(),
        ),
    };

    let mut legs = vec![Leg {
        what: "channels · members",
        node: if self_hosting {
            "Your own server"
        } else {
            "Server"
        },
        where_to: gateway_where,
        seal: gateway_seal,
        note: gateway_note,
        holds_db: true,
        travels: true,
        via: relay_hop.clone(),
    }];

    legs.push(Leg {
        what: "direct messages",
        node: "Nostr relays",
        holds_db: false,
        travels: true,
        via: None,
        where_to: if relays.is_empty() {
            "no relay connected".to_string()
        } else if relays.len() == 1 {
            relays[0].clone()
        } else {
            format!("{} and {} more", relays[0], relays.len() - 1)
        },
        seal: Seal::EndToEnd,
        note: "Nostr relays, never the gateway. Sealed to the recipient's key and wrapped again so the relay cannot see who sent it. The relay stores the ciphertext and cannot be made to forget it.".to_string(),
    });

    if in_voice {
        legs.push(Leg {
            what: "voice · screen",
            node: "SFU",
            holds_db: false,
            travels: true,
            via: None,
            where_to: if voice_bundled {
                "an SFU on this machine".to_string()
            } else {
                "the SFU this server uses".to_string()
            },
            seal: if voice_sealed {
                Seal::EndToEnd
            } else {
                Seal::InTransit
            },
            note: if voice_sealed {
                "Frames are encrypted before they leave. The SFU forwards what it cannot decrypt — including one you run yourself.".to_string()
            } else {
                "No media key for this channel yet. Until one arrives the SFU is trusted with the frames.".to_string()
            },
        });
    }

    // The control plane, and only when there is one. It carries no message and
    // no frame — but it learns things, and a panel about where data goes that
    // omits the machine holding your address is not answering the question.
    if let Some(rz) = rendezvous_label.clone() {
        let listed = host.as_ref().map(|h| h.listed_public).unwrap_or(false);
        legs.push(Leg {
            what: "discovery",
            node: "Rendezvous",
            where_to: rz,
            seal: Seal::InTransit,
            holds_db: false,
            travels: true,
            via: None,
            note: if self_hosting {
                format!(
                    "This machine registered here so a friend can find it by code. It holds your address and your key{}. No message or frame passes through it{}.",
                    if listed {
                        ", and this guild is listed in its public directory"
                    } else {
                        ""
                    },
                    if relay_hop.is_some() {
                        " except while relaying, as above"
                    } else {
                        ""
                    },
                )
            } else {
                "It answered your join code with the host's address, so it knows you asked for that code and it saw your IP. Nothing you say afterwards goes through it unless the connection is relayed.".to_string()
            },
        });
    }

    legs.push(Leg {
        what: "your key",
        node: "Stays here",
        holds_db: false,
        travels: false,
        via: None,
        where_to: "never sent".to_string(),
        seal: Seal::Local,
        note: "The private half never leaves. It signs a login for the exact address you dialled, so a signature for one server is useless to another.".to_string(),
    });

    let you = if self_hosting {
        "host + client"
    } else {
        "this client"
    };
    let diagram = diagram_svg(&legs, you);

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[75] flex items-center justify-center bg-black/50 p-6",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in w-[34rem] max-w-full max-h-full flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e: MouseEvent| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Where your data goes" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-3 space-y-3",
                    div {
                        class: "rounded-lg border border-[var(--edge)] p-2",
                        style: "background: var(--bg2);",
                        dangerous_inner_html: "{diagram}",
                    }
                    div { class: "flex items-center justify-center gap-3 text-[9px] text-[var(--text-dim)] uppercase tracking-wider",
                        for seal in [Seal::EndToEnd, Seal::InTransit, Seal::Local] {
                            {
                                let (label, color) = seal.chip();
                                let dash = match seal {
                                    Seal::EndToEnd => "",
                                    Seal::InTransit => "6 4",
                                    Seal::Local => "2 3",
                                };
                                rsx! {
                                    span { key: "{label}", class: "flex items-center gap-1",
                                        span {
                                            class: "block",
                                            dangerous_inner_html: format!(
                                                "<svg width=\"22\" height=\"6\" viewBox=\"0 0 22 6\"><line x1=\"0\" y1=\"3\" x2=\"22\" y2=\"3\" stroke=\"{color}\" stroke-width=\"1.8\" stroke-dasharray=\"{dash}\"/></svg>"
                                            ),
                                        }
                                        span { style: "color: {color};", "{label}" }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "rounded-lg border border-[var(--edge)] p-2.5", style: "background: var(--bg2);",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1",
                            "The database"
                        }
                        div { class: "text-xs text-[var(--text)] leading-relaxed", "{db_line}" }
                    }

                    for leg in legs {
                        {
                            let (chip, color) = leg.seal.chip();
                            rsx! {
                                div { key: "{leg.node}", class: "rounded-lg border border-[var(--edge)] p-2.5",
                                    div { class: "flex items-center gap-2 mb-1",
                                        span { class: "text-xs font-medium text-[var(--text)] flex-1 truncate",
                                            "{leg.node}"
                                        }
                                        span {
                                            class: "shrink-0 text-[9px] uppercase tracking-wider px-1.5 py-px rounded",
                                            style: "color: {color}; background: color-mix(in srgb, {color} 12%, transparent);",
                                            "{chip}"
                                        }
                                    }
                                    div { class: "text-[11px] text-[var(--text-muted)] leading-relaxed", "{leg.note}" }
                                }
                            }
                        }
                    }

                    div { class: "text-[10px] text-[var(--text-dim)] leading-relaxed pt-1",
                        "\"End to end\" means whoever forwards it cannot read it. \"In transit\" means the wire is encrypted but an endpoint on the path can."
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(what: &'static str, node: &'static str, seal: Seal, db: bool) -> Leg {
        Leg {
            what,
            node,
            where_to: "example.test:443".into(),
            seal,
            note: String::new(),
            holds_db: db,
            travels: true,
            via: None,
        }
    }

    #[test]
    fn a_hop_is_drawn_on_the_arrow_it_actually_sits_on() {
        let mut relayed = leg("channels", "Server", Seal::EndToEnd, false);
        relayed.via = Some("via relay".into());
        let with_hop = diagram_svg(&[relayed], "you");
        assert!(with_hop.contains("via relay"));
        // The pill, on top of the arrow's own box.
        assert_eq!(with_hop.matches("<rect").count(), 3);

        let direct = diagram_svg(&[leg("channels", "Server", Seal::EndToEnd, false)], "you");
        assert_eq!(direct.matches("<rect").count(), 2);
    }

    #[test]
    fn a_leg_that_stays_put_gets_a_caption_rather_than_an_arrow() {
        let mut key = leg("your key", "Stays here", Seal::Local, false);
        key.travels = false;
        let svg = diagram_svg(&[leg("a", "A", Seal::EndToEnd, false), key], "you");
        // One destination box plus the one for "You" — not two destinations.
        assert_eq!(svg.matches("<rect").count(), 2);
        assert_eq!(svg.matches("marker-end").count(), 1);
        assert!(svg.contains("your key never leaves"));
    }

    #[test]
    fn every_leg_gets_a_box_and_an_arrow() {
        let legs = vec![
            leg("channels", "Server", Seal::EndToEnd, true),
            leg("dms", "Nostr relays", Seal::EndToEnd, false),
            leg("key", "Stays here", Seal::Local, false),
        ];
        let svg = diagram_svg(&legs, "this client");
        // Three destination boxes plus the one for "You".
        assert_eq!(svg.matches("<rect").count(), 4);
        // `marker-end` is what makes a path an arrow: the three arrowheads in
        // `<defs>` and the database cylinder are paths too.
        assert_eq!(svg.matches("marker-end").count(), 3);
        assert!(svg.ends_with("</svg>"));
    }

    #[test]
    fn a_readable_leg_is_dashed_as_well_as_coloured() {
        let e2e = diagram_svg(&[leg("a", "A", Seal::EndToEnd, false)], "you");
        assert!(!e2e.contains("stroke-dasharray"));

        let transit = diagram_svg(&[leg("a", "A", Seal::InTransit, false)], "you");
        assert!(transit.contains("stroke-dasharray"));
        assert!(transit.contains("var(--warn)"));
    }

    #[test]
    fn the_cylinder_is_drawn_only_beside_whoever_holds_the_rows() {
        let with = diagram_svg(&[leg("a", "A", Seal::Local, true)], "you");
        let without = diagram_svg(&[leg("a", "A", Seal::Local, false)], "you");
        assert!(with.contains("<ellipse"));
        assert!(!without.contains("<ellipse"));
    }

    #[test]
    fn a_hostile_hostname_cannot_close_the_tag_it_lands_in() {
        let mut l = leg("a", "A", Seal::Local, false);
        l.where_to = "</text><script>x</script>".into();
        let svg = diagram_svg(&[l], "you");
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;/text&gt;"));
    }

    #[test]
    fn a_long_address_loses_its_middle_and_keeps_its_port() {
        let long = "a-very-long-hostname-indeed.example.test:8443";
        let cut = ellipsize(long, 30);
        assert!(cut.chars().count() <= 30);
        assert!(cut.ends_with("8443"), "got {cut}");
        assert!(cut.starts_with("a-very"));
    }

    #[test]
    fn a_short_address_is_left_exactly_as_it_is() {
        assert_eq!(ellipsize("host:443", 30), "host:443");
    }

    /// Not an assertion — a way to look at the thing. `cargo test -p dioxusfun
    /// -- --ignored render_the_diagram --nocapture` writes it to the temp
    /// directory and prints the path, because the diagram is the one part of
    /// this file no assertion can judge.
    #[test]
    #[ignore]
    fn render_the_diagram() {
        let mut key = leg("your key", "Stays here", Seal::Local, false);
        key.travels = false;
        key.where_to = "never sent".into();
        let mut gateway = leg("channels · members", "Server", Seal::EndToEnd, true);
        gateway.where_to = "chat.example.test:443".into();
        gateway.via = Some("via relay".into());
        let mut rz = leg("discovery", "Rendezvous", Seal::InTransit, false);
        rz.where_to = "rz.discordia.test:443".into();
        let legs = vec![
            gateway,
            leg("direct messages", "Nostr relays", Seal::EndToEnd, false),
            leg("voice · screen", "SFU", Seal::InTransit, false),
            rz,
            key,
        ];
        let svg = diagram_svg(&legs, "this client");
        let path = std::env::temp_dir().join("topology-preview.svg");
        std::fs::write(&path, &svg).expect("write preview");
        println!("wrote {} ({} bytes)", path.display(), svg.len());
    }
}
