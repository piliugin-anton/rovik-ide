//! Which pages a command on a team server may open on the user's computer
//! (D20): pages of the user's own servers there, http or https at
//! localhost, 127.0.0.0/8 or ::1. The daemon checks a page before it asks
//! the user's computer to open it (pages.rs), and the computer checks it
//! again before it forwards a port to it and opens it: whatever asks, a
//! page of another address is never opened on the user's computer.

use std::net::IpAddr;

/// The port the page at `url` is served on, when it's a page of this
/// user's own servers: http or https, at localhost, 127.0.0.0/8 or ::1.
/// Why not, in words for the agent, when it isn't.
pub fn local_page(url: &str) -> Result<u16, String> {
    let parsed = reqwest::Url::parse(url.trim()).map_err(|_| {
        format!(
            "Rovik didn't open \"{url}\": it isn't a web address. A command here may open only \
             pages of the servers it runs, like http://localhost:5173, which Rovik opens in the \
             user's browser on their computer."
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "Rovik didn't open {url}: only http and https pages of the servers commands run \
             here are opened, not {}: addresses.",
            parsed.scheme()
        ));
    }
    let host = parsed.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let local = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !local {
        return Err(format!(
            "Rovik refused to open {url}, for the user's safety: a command on a cloud host may \
             open only pages of the servers it runs, at http://localhost or http://127.0.0.1 \
             and a port, which Rovik opens in the user's browser on their computer. If the user \
             needs this address, tell them instead."
        ));
    }
    parsed
        .port_or_known_default()
        .ok_or_else(|| format!("Rovik didn't open {url}: it names no port."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_of_the_users_own_servers_are_local_and_name_their_port() {
        for (url, port) in [
            ("http://localhost:5173/", 5173),
            ("http://LOCALHOST:3000/app?x=1", 3000),
            ("http://127.0.0.1:8080", 8080),
            ("http://127.1.2.3:9000/", 9000),
            ("https://[::1]:8443/", 8443),
            ("http://localhost/", 80),
            ("https://localhost/", 443),
        ] {
            assert_eq!(local_page(url), Ok(port), "{url}");
        }
    }

    #[test]
    fn any_other_address_is_refused_with_why() {
        for url in [
            "https://example.com/",
            "http://10.0.0.5:3000/",
            "http://192.168.1.2:5173",
            "http://0.0.0.0:3000/",
            "http://localhost.example.com:3000/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let why = local_page(url).unwrap_err();
            assert!(
                why.contains("refused") && why.contains("localhost"),
                "{url}: {why}"
            );
        }
        let why = local_page("file:///etc/passwd").unwrap_err();
        assert!(why.contains("not file:"), "{why}");
        let why = local_page("not a url").unwrap_err();
        assert!(why.contains("isn't a web address"), "{why}");
    }
}
