pub use discordia_server::rendezvous::*;

impl Signer for crate::identity::Identity {
    fn pubkey(&self) -> String {
        self.pubkey.clone()
    }

    fn sign_hex(&self, message: &[u8]) -> String {
        crate::identity::Identity::sign_hex(self, message)
    }
}
