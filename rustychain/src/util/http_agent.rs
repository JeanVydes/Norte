/// Factory for generating realistic user agent strings
pub struct UserAgentFactory;

impl UserAgentFactory {
    /// Generates a random Chrome user agent
    pub fn chrome() -> String {
        let versions = ["120.0.0.0", "121.0.0.0", "122.0.0.0", "123.0.0.0"];
        let version = versions[fastrand::usize(..versions.len())];
        format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Safari/537.36",
            version
        )
    }

    /// Generates a random Firefox user agent
    pub fn firefox() -> String {
        let versions = ["120.0", "121.0", "122.0", "123.0"];
        let version = versions[fastrand::usize(..versions.len())];
        format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:{}) Gecko/20100101 Firefox/{}",
            version, version
        )
    }

    /// Generates a random Safari user agent
    pub fn safari() -> String {
        let versions = ["17.1", "17.2", "17.3", "17.4"];
        let version = versions[fastrand::usize(..versions.len())];
        format!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_1) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{} Safari/605.1.15",
            version
        )
    }

    /// Generates a random user agent from available browsers
    pub fn random() -> String {
        match fastrand::usize(..3) {
            0 => Self::chrome(),
            1 => Self::firefox(),
            _ => Self::safari(),
        }
    }
}
