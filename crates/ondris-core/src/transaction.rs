use ondris_primitives::{Address, Hash256, KeyPair, PublicKey, Signature};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transaction {
    pub from: PublicKey,
    pub to: Address,
    pub amount: u64,
    pub fee: u64,
    /// Nonce du compte émetteur, doit être strictement croissant (protection
    /// contre le rejeu), pas lié au nonce de PoW du bloc.
    pub account_nonce: u64,
    pub signature: Option<Signature>,
}

impl Transaction {
    pub fn new_unsigned(
        from: PublicKey,
        to: Address,
        amount: u64,
        fee: u64,
        account_nonce: u64,
    ) -> Self {
        Self {
            from,
            to,
            amount,
            fee,
            account_nonce,
            signature: None,
        }
    }

    /// Octets signés : tout sauf la signature elle-même.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(96);
        buf.extend_from_slice(&self.from.0);
        buf.extend_from_slice(&self.to.0);
        buf.extend_from_slice(&self.amount.to_le_bytes());
        buf.extend_from_slice(&self.fee.to_le_bytes());
        buf.extend_from_slice(&self.account_nonce.to_le_bytes());
        buf
    }

    pub fn sign(&mut self, keypair: &KeyPair) {
        assert_eq!(
            keypair.public().0,
            self.from.0,
            "la clé ne correspond pas à l'émetteur"
        );
        self.signature = Some(keypair.sign(&self.signing_bytes()));
    }

    pub fn is_signature_valid(&self) -> bool {
        match &self.signature {
            Some(sig) => self.from.verify(&self.signing_bytes(), sig),
            None => false,
        }
    }

    pub fn hash(&self) -> Hash256 {
        let bytes = serde_json::to_vec(self)
            .expect("la sérialisation d'une transaction ne peut pas échouer");
        Hash256::hash(&bytes)
    }
}
