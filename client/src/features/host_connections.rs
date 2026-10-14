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
        .map(|route| match route {
            GatewayRoute::Ipv4 => "IPv4",
            GatewayRoute::Ipv6 => "IPv6",
            GatewayRoute::Relay => "Relay",
            GatewayRoute::Local => "LAN",
            GatewayRoute::Unknown => "Unknown",
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let offered = if offered.is_empty() {
        "This PC only".to_owned()
    } else {
        offered
    };
    rsx! {
        button {
            class: "shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--success)] hover:underline",
            title: "View how clients connect to your server",
            aria_expanded: "{open}",
            onclick: move |_| open.toggle(),
            "Server: local · Clients: {total}"
        }
        if open() {
            div {
                role: "dialog",
                aria_label: "Host connections",
                class: "dxf-no-drag rounded-md border border-[var(--border)] bg-[var(--panel2)] p-4 text-[12px]",
                style: "position:fixed;top:60px;right:16px;z-index:1000;max-width:min(520px,90vw);max-height:75vh;overflow:auto;white-space:normal;",
                div { class: "flex items-center justify-between gap-4",
                    strong { "Host connections" }
                    button { onclick: move |_| open.set(false), "Close" }
                }
                p { class: "mt-2", "Advertised routes: {offered}" }
                p { class: "text-[var(--text-muted)]", "Advertising a route does not confirm that someone outside can reach it." }
                p { class: "mt-2", "Active: IPv4 {ipv4} · IPv6 {ipv6} · Relay {relay} · Other {other}" }
                p { class: "text-[var(--text-muted)]", "Observed by this server. Updates every 5 seconds. Your local host session is excluded. Voice and streams use separate routes." }
                if total == 0 {
                    p { class: "mt-3", "No other clients connected." }
                }
                for connection in &info.connections {
                    div { key: "{connection.id}", class: "flex justify-between gap-4 border-t border-[var(--border)] py-2",
                        span { class: "truncate", "{connection.name}" }
                        span { "{connection.route.label()}" }
                    }
                }
            }
        }
    }
}
