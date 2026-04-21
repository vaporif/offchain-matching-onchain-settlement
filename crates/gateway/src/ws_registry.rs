use std::collections::HashMap;

use alloy::primitives::Address;
use tokio::sync::mpsc;

use crate::state::WsMessage;

#[derive(Debug, Default)]
pub struct WsRegistry {
    connections: HashMap<Address, Vec<(usize, mpsc::UnboundedSender<WsMessage>)>>,
    next_id: usize,
}

impl WsRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, address: Address) -> (usize, mpsc::UnboundedReceiver<WsMessage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_id;
        self.next_id += 1;
        self.connections.entry(address).or_default().push((id, tx));
        (id, rx)
    }

    pub fn send_to(&mut self, address: &Address, msg: &WsMessage) {
        if let Some(senders) = self.connections.get_mut(address) {
            senders.retain(|(_, tx)| tx.send(msg.clone()).is_ok());
            if senders.is_empty() {
                self.connections.remove(address);
            }
        }
    }

    pub fn remove(&mut self, address: &Address, sender_id: usize) {
        if let Some(senders) = self.connections.get_mut(address) {
            senders.retain(|(id, _)| *id != sender_id);
            if senders.is_empty() {
                self.connections.remove(address);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_to_delivers_to_registered_address() {
        let mut registry = WsRegistry::new();
        let addr = Address::with_last_byte(1);
        let (_id, mut rx) = registry.register(addr);

        let msg = WsMessage {
            msg_type: "fill".into(),
            data: serde_json::json!({"test": true}),
        };
        registry.send_to(&addr, &msg);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.msg_type, "fill");
    }

    #[tokio::test]
    async fn send_to_does_not_leak_to_other_addresses() {
        let mut registry = WsRegistry::new();
        let addr1 = Address::with_last_byte(1);
        let addr2 = Address::with_last_byte(2);
        let (_id1, mut rx1) = registry.register(addr1);
        let (_id2, mut rx2) = registry.register(addr2);

        let msg = WsMessage {
            msg_type: "fill".into(),
            data: serde_json::json!({"for": "addr1"}),
        };
        registry.send_to(&addr1, &msg);

        assert!(rx1.recv().await.is_some());
        assert!(rx2.try_recv().is_err());
    }

    #[tokio::test]
    async fn remove_disconnects_specific_sender() {
        let mut registry = WsRegistry::new();
        let addr = Address::with_last_byte(1);
        let (id1, _rx1) = registry.register(addr);
        let (_id2, mut rx2) = registry.register(addr);

        registry.remove(&addr, id1);

        let msg = WsMessage {
            msg_type: "fill".into(),
            data: serde_json::json!({}),
        };
        registry.send_to(&addr, &msg);

        assert!(rx2.recv().await.is_some());
    }

    #[tokio::test]
    async fn prunes_dead_senders_on_send() {
        let mut registry = WsRegistry::new();
        let addr = Address::with_last_byte(1);
        let (_id1, rx1) = registry.register(addr);
        let (_id2, mut rx2) = registry.register(addr);

        drop(rx1);

        let msg = WsMessage {
            msg_type: "fill".into(),
            data: serde_json::json!({}),
        };
        registry.send_to(&addr, &msg);

        assert!(rx2.recv().await.is_some());
        assert_eq!(registry.connections[&addr].len(), 1);
    }
}
