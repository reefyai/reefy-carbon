//! Admit authenticated direct messages using account identity, not device identity.
use whatsapp_rust::{prelude::Jid, wacore::types::message::MessageInfo};

pub(crate) fn admit(info: &MessageInfo, unlinking: bool) -> Result<Jid, &'static str> {
    if unlinking {
        return Err("Ignored while WhatsApp is unlinking");
    }
    if info.source.is_from_me {
        return Err("Ignored self-sent message");
    }
    if info.source.is_group {
        return Err("Ignored group or broadcast message");
    }
    if info.is_offline {
        return Err("Ignored offline message");
    }
    if info.ephemeral_expiration.is_some_and(|v| v > 0) {
        return Err("Ignored disappearing message");
    }
    let chat = info.source.chat.to_non_ad();
    if !(chat.is_pn() || chat.is_lid()) {
        return Err("Ignored unsupported chat address");
    }
    // The parser strips device addressing from chat but deliberately retains it
    // on sender for Signal sessions. Compare accounts without weakening DM checks.
    if info.source.sender.to_non_ad() != chat {
        return Err("Ignored sender/chat account mismatch");
    }
    Ok(chat)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use whatsapp_rust::{wacore::messages::parse_message_info, NodeBuilder};

    pub(crate) fn parsed(from: &str, id: &str) -> MessageInfo {
        let node = NodeBuilder::new("message")
            .attr("from", from)
            .attr("id", id)
            .attr("type", "text")
            .attr("t", "1790470000")
            .build();
        parse_message_info(
            &node.as_node_ref(),
            &"15550000000@s.whatsapp.net".parse().unwrap(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn primary_and_linked_devices_share_canonical_account() {
        for account in ["15550000001@s.whatsapp.net", "100000000000001@lid"] {
            for device in [0, 1, 7, 255] {
                let (user, server) = account.split_once('@').unwrap();
                let from = if device == 0 {
                    account.to_owned()
                } else {
                    format!("{user}:{device}@{server}")
                };
                let info = parsed(&from, "SYNTHETIC");
                assert_eq!(admit(&info, false).unwrap().to_string(), account);
                // Demonstrate the precise pre-fix regression, not just the helper's output.
                assert_eq!(
                    info.source.sender.to_string() == info.source.chat.to_string(),
                    device == 0
                );
            }
        }
    }

    #[test]
    fn normalized_identity_does_not_relax_admission_policy() {
        let baseline = parsed("100000000000001:7@lid", "SYNTHETIC");
        assert!(admit(&baseline, true).is_err());
        let mut cases = Vec::new();
        let mut v = baseline.clone();
        v.source.is_from_me = true;
        cases.push(v);
        let mut v = baseline.clone();
        v.source.is_group = true;
        cases.push(v);
        let mut v = baseline.clone();
        v.is_offline = true;
        cases.push(v);
        let mut v = baseline.clone();
        v.ephemeral_expiration = Some(60);
        cases.push(v);
        for other in ["100000000000002@lid", "100000000000001@s.whatsapp.net"] {
            let mut v = baseline.clone();
            v.source.sender = other.parse().unwrap();
            cases.push(v);
        }
        for address in [
            "120363000000001@g.us",
            "status@broadcast",
            "100000000000001@newsletter",
        ] {
            let mut v = baseline.clone();
            v.source.sender = address.parse().unwrap();
            v.source.chat = v.source.sender.clone();
            cases.push(v);
        }
        for v in cases {
            assert!(
                admit(&v, false).is_err(),
                "unexpected admission: {:?}",
                v.source
            );
        }
    }
}
