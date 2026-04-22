//! Webhook signature verification for GitHub webhooks
//!
//! GitHub signs webhook payloads with HMAC-SHA256 using the webhook secret.
//! The signature is sent in the X-Hub-Signature-256 header.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Verify the GitHub webhook signature
///
/// # Arguments
/// * `secret` - The webhook secret configured in GitHub
/// * `signature_header` - The X-Hub-Signature-256 header value (format: "sha256=<hex>")
/// * `payload` - The raw request body bytes
///
/// # Returns
/// * `Ok(())` if signature is valid
/// * `Err(String)` with error message if signature is invalid
pub fn verify_webhook_signature(
    secret: &str,
    signature_header: &str,
    payload: &[u8],
) -> Result<(), String> {
    // Extract the hex signature from the header (format: "sha256=<hex>")
    let signature_hex = signature_header
        .strip_prefix("sha256=")
        .ok_or_else(|| "Signature header missing 'sha256=' prefix".to_string())?;

    // Decode the hex signature
    let expected_signature =
        hex::decode(signature_hex).map_err(|e| format!("Failed to decode signature hex: {}", e))?;

    // Compute HMAC-SHA256 of the payload
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| format!("Failed to create HMAC: {}", e))?;
    mac.update(payload);

    // Verify the signature using constant-time comparison
    mac.verify_slice(&expected_signature)
        .map_err(|_| "Signature verification failed".to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_valid_signature() {
        let secret = "my-secret";
        let payload = b"test payload";

        // Create HMAC signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let signature = mac.finalize().into_bytes();
        let signature_hex = hex::encode(signature);
        let signature_header = format!("sha256={}", signature_hex);

        // Verify
        assert!(verify_webhook_signature(secret, &signature_header, payload).is_ok());
    }

    #[test]
    fn test_verify_invalid_signature() {
        let secret = "my-secret";
        let payload = b"test payload";
        let signature_header =
            "sha256=0000000000000000000000000000000000000000000000000000000000000000";

        let result = verify_webhook_signature(secret, signature_header, payload);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Signature verification failed");
    }

    #[test]
    fn test_verify_wrong_secret() {
        let secret = "my-secret";
        let wrong_secret = "wrong-secret";
        let payload = b"test payload";

        // Create signature with wrong secret
        let mut mac = HmacSha256::new_from_slice(wrong_secret.as_bytes()).unwrap();
        mac.update(payload);
        let signature = mac.finalize().into_bytes();
        let signature_hex = hex::encode(signature);
        let signature_header = format!("sha256={}", signature_hex);

        // Verify with correct secret should fail
        let result = verify_webhook_signature(secret, &signature_header, payload);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Signature verification failed");
    }

    #[test]
    fn test_missing_prefix() {
        let secret = "my-secret";
        let payload = b"test payload";
        let signature_header = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = verify_webhook_signature(secret, signature_header, payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing 'sha256=' prefix"));
    }

    #[test]
    fn test_invalid_hex() {
        let secret = "my-secret";
        let payload = b"test payload";
        let signature_header = "sha256=not-valid-hex";

        let result = verify_webhook_signature(secret, signature_header, payload);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("Failed to decode signature hex")
        );
    }
}
