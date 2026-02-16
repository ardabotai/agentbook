use super::{NodeState, error_response, ok_response};
use agentbook::protocol::{
    ContractReadResult, Response, SignatureResult, TotpSetupInfo, TxResult, WalletInfo, WalletType,
};
use agentbook_wallet::spending_limit::Asset;
use agentbook_wallet::wallet::{self, BaseWallet};
use alloy::primitives::{Address, U256};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Wallet initialization helpers
// ---------------------------------------------------------------------------

/// Get a reference to the human wallet, lazily initializing it if needed.
fn get_human_wallet(state: &Arc<NodeState>) -> Result<&BaseWallet, Response> {
    if let Some(w) = state.human_wallet.get() {
        return Ok(w);
    }
    let key_bytes = state.identity.secret_key_bytes();
    let w = BaseWallet::new(&key_bytes, &state.wallet.rpc_url).map_err(|e| {
        error_response("wallet_error", &format!("failed to init human wallet: {e}"))
    })?;
    // Another thread may have initialized it; that's fine -- just use whichever won.
    let _ = state.human_wallet.set(w);
    Ok(state.human_wallet.get().unwrap())
}

/// Get a reference to the yolo wallet, lazily initializing it if needed.
fn get_yolo_wallet(state: &Arc<NodeState>) -> Result<&BaseWallet, Response> {
    if !state.wallet.yolo_enabled {
        return Err(error_response(
            "wallet_error",
            "yolo mode is not enabled -- start with --yolo",
        ));
    }
    if let Some(w) = state.yolo_wallet.get() {
        return Ok(w);
    }
    let key_bytes = agentbook_wallet::yolo::load_yolo_key(&state.wallet.state_dir)
        .map_err(|e| error_response("wallet_error", &format!("failed to load yolo key: {e}")))?;
    let w = BaseWallet::new(&key_bytes, &state.wallet.rpc_url)
        .map_err(|e| error_response("wallet_error", &format!("failed to init yolo wallet: {e}")))?;
    let _ = state.yolo_wallet.set(w);
    Ok(state.yolo_wallet.get().unwrap())
}

/// Verify TOTP and return a reference to the human wallet.
/// Combines the two most common preamble steps for authenticated wallet operations.
fn with_human_wallet<'a>(state: &'a Arc<NodeState>, otp: &str) -> Result<&'a BaseWallet, Response> {
    verify_totp(state, otp)?;
    get_human_wallet(state)
}

/// Verify a TOTP code. Returns `Ok(())` on success or an error `Response`.
fn verify_totp(state: &Arc<NodeState>, otp: &str) -> Result<(), Response> {
    match agentbook_wallet::totp::verify_totp(&state.wallet.state_dir, otp, &state.wallet.kek) {
        Ok(true) => Ok(()),
        Ok(false) => Err(error_response("invalid_otp", "invalid authenticator code")),
        Err(e) => Err(error_response(
            "totp_error",
            &format!("TOTP verification failed: {e}"),
        )),
    }
}

/// Parse an Ethereum address string, returning an error Response on failure.
fn parse_address(addr: &str) -> Result<Address, Response> {
    addr.parse()
        .map_err(|e| error_response("invalid_address", &format!("invalid address: {e}")))
}

/// Check yolo spending limits and record the spend if allowed.
/// Returns `Ok(())` if the spend is within limits, or an error `Response` if not.
async fn check_yolo_limit(
    state: &Arc<NodeState>,
    asset: Asset,
    amount: U256,
) -> Result<(), Response> {
    let mut limiter = state.spending_limiter.lock().await;
    limiter
        .check_and_record(asset, amount)
        .map_err(|e| error_response("spending_limit", &e.to_string()))
}

/// Parse a message string: if it starts with 0x, treat as hex bytes; otherwise UTF-8.
fn parse_message_bytes(message: &str) -> Vec<u8> {
    if let Some(hex) = message.strip_prefix("0x") {
        alloy::hex::decode(hex).unwrap_or_else(|_| message.as_bytes().to_vec())
    } else {
        message.as_bytes().to_vec()
    }
}

/// Build a `TxResult` response from a transaction hash.
fn tx_result_response(tx_hash: alloy::primitives::B256) -> Response {
    let result = TxResult {
        tx_hash: format!("{tx_hash:#x}"),
        explorer_url: wallet::explorer_url(&tx_hash),
    };
    ok_response(Some(serde_json::to_value(result).unwrap()))
}

/// Resolve a wallet reference by type.
fn resolve_wallet(
    state: &Arc<NodeState>,
    wallet_type: WalletType,
) -> Result<&BaseWallet, Response> {
    match wallet_type {
        WalletType::Human => get_human_wallet(state),
        WalletType::Yolo => get_yolo_wallet(state),
    }
}

// ---------------------------------------------------------------------------
// Balance
// ---------------------------------------------------------------------------

pub async fn handle_wallet_balance(state: &Arc<NodeState>, wallet_type: WalletType) -> Response {
    let w = match resolve_wallet(state, wallet_type) {
        Ok(w) => w,
        Err(resp) => return resp,
    };

    // Fetch ETH and USDC balances in parallel.
    let (eth_result, usdc_result) = tokio::join!(w.get_eth_balance(), w.get_usdc_balance());

    match (eth_result, usdc_result) {
        (Ok(eth), Ok(usdc)) => {
            let info = WalletInfo {
                address: format!("{:#x}", w.address()),
                eth_balance: wallet::format_eth(eth),
                usdc_balance: wallet::format_usdc(usdc),
                wallet_type,
            };
            ok_response(Some(serde_json::to_value(info).unwrap()))
        }
        (Err(e), _) | (_, Err(e)) => {
            error_response("balance_error", &format!("failed to fetch balance: {e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Send ETH / USDC
// ---------------------------------------------------------------------------

pub async fn handle_send_eth(
    state: &Arc<NodeState>,
    to: &str,
    amount: &str,
    otp: &str,
) -> Response {
    let w = match with_human_wallet(state, otp) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    send_eth(w, to, amount).await
}

pub async fn handle_yolo_send_eth(state: &Arc<NodeState>, to: &str, amount: &str) -> Response {
    let w = match get_yolo_wallet(state) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    let amount_wei = match wallet::parse_eth_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };
    if let Err(resp) = check_yolo_limit(state, Asset::Eth, amount_wei).await {
        return resp;
    }
    let to_addr = match parse_address(to) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match w.send_eth(to_addr, amount_wei).await {
        Ok(tx_hash) => tx_result_response(tx_hash),
        Err(e) => error_response("send_failed", &format!("ETH send failed: {e}")),
    }
}

async fn send_eth(w: &BaseWallet, to: &str, amount: &str) -> Response {
    let to_addr = match parse_address(to) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let amount_wei = match wallet::parse_eth_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };

    match w.send_eth(to_addr, amount_wei).await {
        Ok(tx_hash) => tx_result_response(tx_hash),
        Err(e) => error_response("send_failed", &format!("ETH send failed: {e}")),
    }
}

pub async fn handle_send_usdc(
    state: &Arc<NodeState>,
    to: &str,
    amount: &str,
    otp: &str,
) -> Response {
    let w = match with_human_wallet(state, otp) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    send_usdc(w, to, amount).await
}

pub async fn handle_yolo_send_usdc(state: &Arc<NodeState>, to: &str, amount: &str) -> Response {
    let w = match get_yolo_wallet(state) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    let amount_units = match wallet::parse_usdc_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };
    if let Err(resp) = check_yolo_limit(state, Asset::Usdc, amount_units).await {
        return resp;
    }
    let to_addr = match parse_address(to) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match w.send_usdc(to_addr, amount_units).await {
        Ok(tx_hash) => tx_result_response(tx_hash),
        Err(e) => error_response("send_failed", &format!("USDC send failed: {e}")),
    }
}

async fn send_usdc(w: &BaseWallet, to: &str, amount: &str) -> Response {
    let to_addr = match parse_address(to) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let amount_units = match wallet::parse_usdc_amount(amount) {
        Ok(a) => a,
        Err(e) => return error_response("invalid_amount", &format!("invalid amount: {e}")),
    };

    match w.send_usdc(to_addr, amount_units).await {
        Ok(tx_hash) => tx_result_response(tx_hash),
        Err(e) => error_response("send_failed", &format!("USDC send failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// TOTP setup / verify
// ---------------------------------------------------------------------------

pub async fn handle_setup_totp(state: &Arc<NodeState>) -> Response {
    if agentbook_wallet::totp::has_totp(&state.wallet.state_dir) {
        return error_response("already_configured", "TOTP is already configured");
    }

    match agentbook_wallet::totp::generate_totp_secret(
        &state.wallet.state_dir,
        &state.wallet.kek,
        &state.identity.node_id,
    ) {
        Ok(setup) => {
            let info = TotpSetupInfo {
                secret_base32: setup.secret_base32,
                otpauth_url: setup.otpauth_url,
            };
            ok_response(Some(serde_json::to_value(info).unwrap()))
        }
        Err(e) => error_response("setup_failed", &format!("TOTP setup failed: {e}")),
    }
}

pub async fn handle_verify_totp(state: &Arc<NodeState>, code: &str) -> Response {
    match verify_totp(state, code) {
        Ok(()) => ok_response(Some(serde_json::json!({ "verified": true }))),
        Err(resp) => resp,
    }
}

// ---------------------------------------------------------------------------
// Contract read / write
// ---------------------------------------------------------------------------

pub async fn handle_read_contract(
    state: &Arc<NodeState>,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
) -> Response {
    let address = match parse_address(contract) {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let provider = match state.get_read_provider() {
        Ok(p) => p,
        Err(e) => return error_response("provider_error", &e),
    };

    match agentbook_wallet::contract::read_contract_with_provider(
        provider, address, abi, function, args,
    )
    .await
    {
        Ok(result) => {
            let data = ContractReadResult { result };
            ok_response(Some(serde_json::to_value(data).unwrap()))
        }
        Err(e) => error_response("contract_error", &format!("read_contract failed: {e}")),
    }
}

pub async fn handle_write_contract(
    state: &Arc<NodeState>,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
    value: Option<&str>,
    otp: &str,
) -> Response {
    let w = match with_human_wallet(state, otp) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    write_contract(w, contract, abi, function, args, value).await
}

pub async fn handle_yolo_write_contract(
    state: &Arc<NodeState>,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
    value: Option<&str>,
) -> Response {
    let w = match get_yolo_wallet(state) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    // Enforce ETH spending limit if sending value with the contract call
    if let Some(v) = value {
        let eth_value = match wallet::parse_eth_amount(v) {
            Ok(a) => a,
            Err(e) => return error_response("invalid_value", &format!("invalid ETH value: {e}")),
        };
        if !eth_value.is_zero()
            && let Err(resp) = check_yolo_limit(state, Asset::Eth, eth_value).await
        {
            return resp;
        }
    }
    write_contract(w, contract, abi, function, args, value).await
}

async fn write_contract(
    w: &BaseWallet,
    contract: &str,
    abi: &str,
    function: &str,
    args: &[serde_json::Value],
    value: Option<&str>,
) -> Response {
    let address = match parse_address(contract) {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let eth_value = match value {
        Some(v) => match wallet::parse_eth_amount(v) {
            Ok(a) => Some(a),
            Err(e) => return error_response("invalid_value", &format!("invalid ETH value: {e}")),
        },
        None => None,
    };

    match agentbook_wallet::contract::write_contract(w, address, abi, function, args, eth_value)
        .await
    {
        Ok(tx_hash) => tx_result_response(tx_hash),
        Err(e) => error_response("contract_error", &format!("write_contract failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Message signing
// ---------------------------------------------------------------------------

pub async fn handle_sign_message(state: &Arc<NodeState>, message: &str, otp: &str) -> Response {
    let w = match with_human_wallet(state, otp) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    sign_message(w, message)
}

pub async fn handle_yolo_sign_message(state: &Arc<NodeState>, message: &str) -> Response {
    let w = match get_yolo_wallet(state) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    sign_message(w, message)
}

fn sign_message(w: &BaseWallet, message: &str) -> Response {
    let msg_bytes = parse_message_bytes(message);
    match w.sign_message(&msg_bytes) {
        Ok(sig) => {
            let result = SignatureResult {
                signature: sig,
                address: format!("{:#x}", w.address()),
            };
            ok_response(Some(serde_json::to_value(result).unwrap()))
        }
        Err(e) => error_response("sign_error", &format!("signing failed: {e}")),
    }
}
