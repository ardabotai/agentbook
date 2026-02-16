use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{Address, B256, U256},
    providers::{
        Identity, Provider, ProviderBuilder, RootProvider,
        fillers::{
            BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller,
            WalletFiller,
        },
    },
    rpc::types::TransactionRequest,
    signers::{SignerSync, local::PrivateKeySigner},
    sol,
};
use anyhow::{Context, Result, bail};

/// Base chain ID.
pub const BASE_CHAIN_ID: u64 = 8453;

/// Default RPC endpoint for Base mainnet.
pub const DEFAULT_RPC_URL: &str = "https://mainnet.base.org";

/// USDC contract address on Base (6 decimals).
pub const USDC_ADDRESS: Address =
    alloy::primitives::address!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");

/// USDC has 6 decimal places.
pub const USDC_DECIMALS: u8 = 6;

/// ETH has 18 decimal places.
pub const ETH_DECIMALS: u8 = 18;

/// Basescan base URL for transaction links.
pub const BASESCAN_TX_URL: &str = "https://basescan.org/tx/";

sol! {
    #[sol(rpc)]
    contract IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function transfer(address to, uint256 amount) external returns (bool);
    }
}

/// The concrete provider type returned by `ProviderBuilder::new().wallet(..).connect_http(..)`.
type WalletProvider = FillProvider<
    JoinFill<
        JoinFill<
            Identity,
            JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
        >,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider,
    Ethereum,
>;

/// A wallet for Base chain operations (ETH + USDC).
pub struct BaseWallet {
    provider: WalletProvider,
    address: Address,
    signer: PrivateKeySigner,
}

impl BaseWallet {
    /// Create a new Base wallet from raw secret key bytes.
    pub fn new(secret_key_bytes: &[u8; 32], rpc_url: &str) -> Result<Self> {
        let signer = PrivateKeySigner::from_slice(secret_key_bytes)
            .context("failed to create signer from secret key")?;
        let address = signer.address();
        let signer_clone = signer.clone();

        let provider = ProviderBuilder::new()
            .wallet(signer)
            .connect_http(rpc_url.parse().context("invalid RPC URL")?);

        Ok(Self {
            provider,
            address,
            signer: signer_clone,
        })
    }

    /// Get the wallet's EVM address.
    pub fn address(&self) -> Address {
        self.address
    }

    /// Get the wallet's ETH balance.
    pub async fn get_eth_balance(&self) -> Result<U256> {
        self.provider
            .get_balance(self.address)
            .await
            .context("failed to fetch ETH balance")
    }

    /// Get the wallet's USDC balance.
    pub async fn get_usdc_balance(&self) -> Result<U256> {
        let contract = IERC20::new(USDC_ADDRESS, &self.provider);
        let balance = contract
            .balanceOf(self.address)
            .call()
            .await
            .context("failed to fetch USDC balance")?;
        Ok(balance)
    }

    /// Send ETH to an address. Returns the transaction hash.
    pub async fn send_eth(&self, to: Address, amount_wei: U256) -> Result<B256> {
        let tx = TransactionRequest::default()
            .with_to(to)
            .with_value(amount_wei)
            .with_chain_id(BASE_CHAIN_ID);

        let pending = self
            .provider
            .send_transaction(tx)
            .await
            .context("failed to send ETH transaction")?;

        let tx_hash = *pending.tx_hash();
        tracing::info!(%tx_hash, "ETH transaction sent, waiting for confirmation");

        pending
            .get_receipt()
            .await
            .context("failed to get transaction receipt")?;

        Ok(tx_hash)
    }

    /// Send USDC to an address. Returns the transaction hash.
    pub async fn send_usdc(&self, to: Address, amount: U256) -> Result<B256> {
        let contract = IERC20::new(USDC_ADDRESS, &self.provider);
        let tx_hash = contract
            .transfer(to, amount)
            .send()
            .await
            .context("failed to send USDC transfer")?
            .watch()
            .await
            .context("failed to confirm USDC transfer")?;

        Ok(tx_hash)
    }

    /// EIP-191 personal_sign: sign an arbitrary message and return hex-encoded signature.
    pub fn sign_message(&self, message: &[u8]) -> Result<String> {
        let sig = self.signer.sign_message_sync(message)?;
        Ok(format!("0x{}", alloy::hex::encode(sig.as_bytes())))
    }

    /// Get a reference to the underlying provider (for contract interactions).
    pub fn provider(&self) -> &WalletProvider {
        &self.provider
    }
}

/// Parse a human-readable ETH amount (e.g. "0.01") to wei (U256).
pub fn parse_eth_amount(s: &str) -> Result<U256> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty ETH amount");
    }
    parse_decimal_to_units(s, ETH_DECIMALS).context("invalid ETH amount")
}

/// Parse a human-readable USDC amount (e.g. "10.00") to 6-decimal units (U256).
pub fn parse_usdc_amount(s: &str) -> Result<U256> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty USDC amount");
    }
    parse_decimal_to_units(s, USDC_DECIMALS).context("invalid USDC amount")
}

/// Format wei as human-readable ETH string (e.g. "0.0542 ETH").
pub fn format_eth(wei: U256) -> String {
    let s = format_units(wei, ETH_DECIMALS);
    format!("{s} ETH")
}

/// Format 6-decimal USDC units as human-readable string (e.g. "125.50 USDC").
pub fn format_usdc(units: U256) -> String {
    let s = format_units(units, USDC_DECIMALS);
    format!("{s} USDC")
}

/// Build a basescan explorer URL for a transaction hash.
pub fn explorer_url(tx_hash: &B256) -> String {
    format!("{BASESCAN_TX_URL}{tx_hash:#x}")
}

/// Parse a decimal string like "1.5" into smallest units given decimal places.
fn parse_decimal_to_units(s: &str, decimals: u8) -> Result<U256> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 2 {
        bail!("invalid decimal format: too many dots");
    }

    let whole = parts[0];
    let frac = if parts.len() == 2 { parts[1] } else { "" };

    if frac.len() > decimals as usize {
        bail!(
            "too many decimal places: got {}, max {decimals}",
            frac.len()
        );
    }

    // Pad fractional part to full decimals
    let padded_frac = format!("{frac:0<width$}", width = decimals as usize);
    let combined = format!("{whole}{padded_frac}");

    // Remove leading zeros for parsing (but keep at least one digit)
    let combined = combined.trim_start_matches('0');
    let combined = if combined.is_empty() { "0" } else { combined };

    U256::from_str_radix(combined, 10).context("failed to parse amount as number")
}

/// Format a U256 in smallest units to a human-readable decimal string.
fn format_units(value: U256, decimals: u8) -> String {
    let divisor = U256::from(10u64).pow(U256::from(decimals));
    let whole = value / divisor;
    let frac = value % divisor;

    if frac.is_zero() {
        format!("{whole}.00")
    } else {
        let frac_str = format!("{frac:0>width$}", width = decimals as usize);
        // Trim trailing zeros but keep at least 2 decimal places
        let trimmed = frac_str.trim_end_matches('0');
        let trimmed = if trimmed.len() < 2 {
            &frac_str[..2]
        } else {
            trimmed
        };
        format!("{whole}.{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_eth_whole_number() {
        let wei = parse_eth_amount("1").unwrap();
        assert_eq!(wei, U256::from(1_000_000_000_000_000_000u64));
    }

    #[test]
    fn parse_eth_fractional() {
        let wei = parse_eth_amount("0.01").unwrap();
        assert_eq!(wei, U256::from(10_000_000_000_000_000u64));
    }

    #[test]
    fn parse_eth_small() {
        let wei = parse_eth_amount("0.001").unwrap();
        assert_eq!(wei, U256::from(1_000_000_000_000_000u64));
    }

    #[test]
    fn parse_usdc_whole() {
        let units = parse_usdc_amount("10").unwrap();
        assert_eq!(units, U256::from(10_000_000u64));
    }

    #[test]
    fn parse_usdc_fractional() {
        let units = parse_usdc_amount("10.50").unwrap();
        assert_eq!(units, U256::from(10_500_000u64));
    }

    #[test]
    fn parse_usdc_max_decimals() {
        let units = parse_usdc_amount("1.123456").unwrap();
        assert_eq!(units, U256::from(1_123_456u64));
    }

    #[test]
    fn parse_usdc_too_many_decimals() {
        assert!(parse_usdc_amount("1.1234567").is_err());
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse_eth_amount("").is_err());
    }

    #[test]
    fn format_eth_value() {
        let wei = U256::from(54_200_000_000_000_000u64);
        assert_eq!(format_eth(wei), "0.0542 ETH");
    }

    #[test]
    fn format_eth_whole() {
        let wei = U256::from(1_000_000_000_000_000_000u64);
        assert_eq!(format_eth(wei), "1.00 ETH");
    }

    #[test]
    fn format_usdc_value() {
        let units = U256::from(125_500_000u64);
        assert_eq!(format_usdc(units), "125.50 USDC");
    }

    #[test]
    fn format_usdc_whole() {
        let units = U256::from(10_000_000u64);
        assert_eq!(format_usdc(units), "10.00 USDC");
    }

    #[test]
    fn format_usdc_small() {
        let units = U256::from(1_000u64);
        assert_eq!(format_usdc(units), "0.001 USDC");
    }

    #[test]
    fn explorer_url_format() {
        let hash = B256::ZERO;
        let url = explorer_url(&hash);
        assert!(url.starts_with("https://basescan.org/tx/"));
    }

    #[test]
    fn parse_format_round_trip_eth() {
        let wei = parse_eth_amount("1.5").unwrap();
        let formatted = format_eth(wei);
        assert_eq!(formatted, "1.50 ETH");
    }

    #[test]
    fn parse_format_round_trip_usdc() {
        let units = parse_usdc_amount("100.25").unwrap();
        let formatted = format_usdc(units);
        assert_eq!(formatted, "100.25 USDC");
    }
}
