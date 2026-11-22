mod protocol;
mod session;
mod targets;

pub(in crate::browser) use session::{CdpSession, wait_for_event};
pub(in crate::browser) use targets::{
    Tab, close_tab, connect_to, list_targets, open_tab, pick_tab,
};

#[cfg(test)]
pub(in crate::browser) use targets::encode_query;
