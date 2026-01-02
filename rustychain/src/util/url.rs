use url::Url;

/// URL validator and sanitizer for search results
pub struct UrlValidator;

impl UrlValidator {
    /// Validates that a URL is safe and well-formed
    ///
    /// Returns `None` if the URL is invalid or uses a disallowed scheme
    pub fn validate(url: &str) -> Option<String> {
        // Parse the URL
        let parsed = Url::parse(url).ok()?;

        // Only allow HTTP and HTTPS schemes
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            log::trace!("Rejected URL with invalid scheme: {}", parsed.scheme());
            return None;
        }

        // Reject localhost and private IP ranges (SSRF protection)
        if let Some(host) = parsed.host_str()
            && Self::is_private_or_local(host)
        {
            log::trace!("Rejected private/local URL: {}", host);
            return None;
        }

        Some(parsed.to_string())
    }

    /// Checks if a hostname is localhost or a private IP
    fn is_private_or_local(host: &str) -> bool {
        // Check for localhost
        if host == "localhost" || host == "127.0.0.1" || host == "::1" {
            return true;
        }

        // Check for private IP ranges
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            match ip {
                std::net::IpAddr::V4(ipv4) => {
                    // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                    let octets = ipv4.octets();
                    octets[0] == 10
                        || (octets[0] == 172 && (16..32).contains(&octets[1]))
                        || (octets[0] == 192 && octets[1] == 168)
                        || octets[0] == 127
                }
                std::net::IpAddr::V6(ipv6) => {
                    // Link-local, loopback
                    ipv6.is_loopback() || (ipv6.segments()[0] & 0xffc0) == 0xfe80
                }
            }
        } else {
            false
        }
    }

    /// Sanitizes a URL by removing tracking parameters
    pub fn sanitize(url: &str) -> Option<String> {
        let mut parsed = Url::parse(url).ok()?;

        // Remove common tracking parameters
        let tracking_params = [
            "utm_source",
            "utm_medium",
            "utm_campaign",
            "utm_term",
            "utm_content",
            "fbclid",
            "gclid",
            "msclkid",
            "ref",
            "referrer",
        ];

        {
            let query_pairs: Vec<_> = parsed
                .query_pairs()
                .filter(|(key, _)| !tracking_params.contains(&key.as_ref()))
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();

            if query_pairs.is_empty() {
                parsed.set_query(None);
            } else {
                let query_string = query_pairs
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join("&");
                parsed.set_query(Some(&query_string));
            }
        }

        Some(parsed.to_string())
    }
}
