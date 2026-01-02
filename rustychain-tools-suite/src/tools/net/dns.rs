//! dns
use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use trust_dns_resolver::Resolver;
use trust_dns_resolver::config::*;
use trust_dns_resolver::proto::rr::RecordType;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct DnsArgs {
    #[schemars(description = "The domain name to lookup.")]
    pub domain: String,
    #[schemars(
        description = "The record type: 'A', 'AAAA', 'MX', 'TXT', 'NS', 'CNAME', 'SOA'. Defaults to 'A'."
    )]
    pub record_type: Option<String>,
}

#[derive(Clone, Default)]
pub struct DnsTool;

#[derive(Debug, Serialize, Deserialize)]
pub struct DnsResult {
    pub domain: String,
    pub record_type: String,
    pub records: Vec<String>,
}

#[async_trait::async_trait]
impl FnExecutor<DnsArgs, DnsResult> for DnsTool {
    async fn call(&self, args: DnsArgs) -> rustychain::Result<DnsResult> {
        let resolver = Resolver::new(ResolverConfig::default(), ResolverOpts::default())?;

        let r_type_str = args
            .record_type
            .unwrap_or_else(|| "A".to_string())
            .to_uppercase();

        let r_type = match r_type_str.as_str() {
            "A" => RecordType::A,
            "AAAA" => RecordType::AAAA,
            "MX" => RecordType::MX,
            "TXT" => RecordType::TXT,
            "NS" => RecordType::NS,
            "CNAME" => RecordType::CNAME,
            "SOA" => RecordType::SOA,
            "PTR" => RecordType::PTR,
            "SRV" => RecordType::SRV,
            "CAA" => RecordType::CAA,
            _ => {
                return Err(rustychain::Error::Input(format!(
                    "Unsupported record type: {}",
                    r_type_str
                )));
            }
        };

        let lookup = resolver
            .lookup(&args.domain, r_type)
            .map_err(|e| rustychain::Error::Internal(format!("DNS lookup failed: {}", e).into()))?;

        let records = lookup.iter().map(|data| data.to_string()).collect();

        Ok(DnsResult {
            domain: args.domain,
            record_type: r_type_str,
            records,
        })
    }
}

impl FnDeclarator<DnsArgs, DnsResult> for DnsTool {
    fn declare(&self) -> FunctionDeclaration<DnsArgs, DnsResult> {
        FunctionDeclaration {
            name: "dns_resolver_tool",
            description: "Quickly resolves DNS records using the specified type.",
            parameters: schema_for!(DnsArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
