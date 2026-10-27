use dioxus::prelude::*;
use discordia_server::connection_routes::GatewayRoute;

#[component]
pub(super) fn HostConnections(info: crate::host::HostInfo) -> Element {
    let mut open = use_signal(|| false);
    let total = info.connections.len();
    let count = |route| {
        info.connections
            .iter()
            .filter(|connection| connection.route == route)
            .count()
    };
    let ipv4 = count(GatewayRoute::Ipv4);
    let ipv6 = count(GatewayRoute::Ipv6);
    let relay = count(GatewayRoute::Relay);
    let other = total - ipv4 - ipv6 - relay;
    let offered = info
        .offered_routes
        .iter()
        .map(|route| {
            crate::i18n::label(match route {
                GatewayRoute::Ipv4 => "IPv4",
                GatewayRoute::Ipv6 => "IPv6",
                GatewayRoute::Relay => "Relay",
                GatewayRoute::Local => "LAN",
                GatewayRoute::Unknown => "Unknown",
            })
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let offered = if offered.is_empty() {
        crate::i18n::text("This PC only", &[])
    } else {
        offered
    };
    rsx! {
        button {
            class: "shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--success)] hover:underline",
            title: crate::i18n::text("View how clients connect to your server", &[]),
            aria_expanded: "{open}",
            onclick: move |_| open.toggle(),
            {crate::i18n::text("Server: local · Clients: {0}", &[(total).to_string()])}
        }
        if open() {
            div {
                role: "dialog",
                aria_label: crate::i18n::text("Host connections", &[]),
                class: "dxf-no-drag rounded-md border border-[var(--border)] bg-[var(--panel2)] p-4 text-[12px]",
                style: "position:fixed;top:60px;right:16px;z-index:1000;max-width:min(520px,90vw);max-height:75vh;overflow:auto;white-space:normal;",
                div { class: "flex items-center justify-between gap-4",
                    strong { {crate::i18n::text("Host connections", &[])} }
                    button { onclick: move |_| open.set(false), {crate::i18n::text("Close", &[])} }
                }
                p { class: "mt-2", {crate::i18n::text("Advertised routes: {0}", std::slice::from_ref(&(offered)))} }
                p { class: "text-[var(--text-muted)]", {crate::i18n::text("Advertising a route does not confirm that someone outside can reach it.", &[])} }
                p { class: "mt-2", {crate::i18n::text("Active: IPv4 {0} · IPv6 {1} · Relay {2} · Other {3}", &[(ipv4).to_string(), (ipv6).to_string(), (relay).to_string(), (other).to_string()])} }
                p { class: "text-[var(--text-muted)]", {crate::i18n::text("Observed by this server. Updates every 5 seconds. Your local host session is excluded. Voice and streams use separate routes.", &[])} }
                if total == 0 {
                    p { class: "mt-3", {crate::i18n::text("No other clients connected.", &[])} }
                }
                for connection in &info.connections {
                    div { key: "{connection.id}", class: "flex justify-between gap-4 border-t border-[var(--border)] py-2",
                        span { class: "truncate", "{connection.name}" }
                        span { {crate::i18n::label(connection.route.label())} }
                    }
                }
            }
        }
    }
}
