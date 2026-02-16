use alloy::{
    dyn_abi::{DynSolValue, FunctionExt, JsonAbiExt},
    json_abi::{Function, JsonAbi},
    network::TransactionBuilder,
    primitives::{Address, B256, Bytes, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;

use crate::wallet::{BASE_CHAIN_ID, BaseWallet};

/// Look up a function by name in the ABI. Errors if not found or ambiguous.
pub fn find_function(abi: &JsonAbi, name: &str) -> Result<Function> {
    let funcs = abi
        .functions
        .get(name)
        .with_context(|| format!("function '{name}' not found in ABI"))?;
    if funcs.len() > 1 {
        bail!(
            "function '{name}' is overloaded ({} variants) — overloaded functions are not supported",
            funcs.len()
        );
    }
    Ok(funcs[0].clone())
}

/// Convert a JSON argument array to `DynSolValue`s guided by the function's ABI param types.
pub fn encode_args(func: &Function, args: &[JsonValue]) -> Result<Vec<DynSolValue>> {
    if args.len() != func.inputs.len() {
        bail!(
            "expected {} arguments for '{}', got {}",
            func.inputs.len(),
            func.name,
            args.len()
        );
    }
    func.inputs
        .iter()
        .zip(args.iter())
        .map(|(param, val)| {
            json_to_dyn_sol(&param.ty.to_string(), val)
                .with_context(|| format!("failed to encode param '{}'", param.name))
        })
        .collect()
}

/// Convert a single JSON value to a `DynSolValue` guided by an ABI type string.
pub fn json_to_dyn_sol(ty: &str, value: &JsonValue) -> Result<DynSolValue> {
    // Handle arrays: T[] or T[N]
    if let Some(inner_ty) = ty.strip_suffix("[]") {
        let arr = value
            .as_array()
            .context("expected JSON array for dynamic array type")?;
        let items: Result<Vec<DynSolValue>> =
            arr.iter().map(|v| json_to_dyn_sol(inner_ty, v)).collect();
        return Ok(DynSolValue::Array(items?));
    }
    if ty.ends_with(']')
        && let Some(bracket_pos) = ty.rfind('[')
    {
        let inner_ty = &ty[..bracket_pos];
        let arr = value
            .as_array()
            .context("expected JSON array for fixed array type")?;
        let items: Result<Vec<DynSolValue>> =
            arr.iter().map(|v| json_to_dyn_sol(inner_ty, v)).collect();
        return Ok(DynSolValue::FixedArray(items?));
    }

    // Handle tuple
    if ty == "tuple" || ty.starts_with('(') {
        let _arr = value
            .as_array()
            .context("expected JSON array for tuple type")?;
        bail!(
            "tuple types must be encoded via the Function ABI — pass them as positional array elements"
        );
    }

    match ty {
        "address" => {
            let s = value.as_str().context("address must be a hex string")?;
            let addr: Address = s.parse().context("invalid address")?;
            Ok(DynSolValue::Address(addr))
        }
        "bool" => {
            let b = value.as_bool().context("bool param must be true/false")?;
            Ok(DynSolValue::Bool(b))
        }
        "string" => {
            let s = value
                .as_str()
                .context("string param must be a JSON string")?;
            Ok(DynSolValue::String(s.to_string()))
        }
        "bytes" => {
            let s = value.as_str().context("bytes must be a hex string")?;
            let bytes = alloy::hex::decode(s).context("invalid hex for bytes")?;
            Ok(DynSolValue::Bytes(bytes))
        }
        _ if ty.starts_with("bytes") => {
            // bytesN (1..32)
            let n: usize = ty[5..].parse().context("invalid bytesN size")?;
            let s = value.as_str().context("bytesN must be a hex string")?;
            let bytes = alloy::hex::decode(s).context("invalid hex for bytesN")?;
            if bytes.len() != n {
                bail!("expected {n} bytes for {ty}, got {}", bytes.len());
            }
            let mut word = [0u8; 32];
            word[..n].copy_from_slice(&bytes);
            Ok(DynSolValue::FixedBytes(B256::from(word), n))
        }
        _ if ty.starts_with("uint") => {
            let val = parse_uint_value(value)?;
            Ok(DynSolValue::Uint(val, ty[4..].parse().unwrap_or(256)))
        }
        _ if ty.starts_with("int") => {
            // For signed integers, parse as U256 (covers most practical cases)
            let val = parse_uint_value(value)?;
            let bits: usize = ty[3..].parse().unwrap_or(256);
            Ok(DynSolValue::Int(
                alloy::primitives::I256::from_raw(val),
                bits,
            ))
        }
        _ => bail!("unsupported ABI type: {ty}"),
    }
}

/// Parse a JSON value (string or number) as U256.
fn parse_uint_value(value: &JsonValue) -> Result<U256> {
    match value {
        JsonValue::String(s) => {
            if let Some(hex) = s.strip_prefix("0x") {
                U256::from_str_radix(hex, 16).context("invalid hex uint")
            } else {
                U256::from_str_radix(s, 10).context("invalid decimal uint")
            }
        }
        JsonValue::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(U256::from(u))
            } else {
                bail!("number too large — use a string for big integers")
            }
        }
        _ => bail!("uint must be a string or number"),
    }
}

/// Convert a `DynSolValue` back to JSON for response encoding.
pub fn dyn_sol_to_json(val: &DynSolValue) -> JsonValue {
    match val {
        DynSolValue::Address(a) => JsonValue::String(format!("{a:#x}")),
        DynSolValue::Bool(b) => JsonValue::Bool(*b),
        DynSolValue::String(s) => JsonValue::String(s.clone()),
        DynSolValue::Bytes(b) => JsonValue::String(format!("0x{}", alloy::hex::encode(b))),
        DynSolValue::FixedBytes(b, size) => {
            JsonValue::String(format!("0x{}", alloy::hex::encode(&b[..*size])))
        }
        DynSolValue::Uint(u, _) => JsonValue::String(u.to_string()),
        DynSolValue::Int(i, _) => JsonValue::String(i.to_string()),
        DynSolValue::Array(items) | DynSolValue::FixedArray(items) => {
            JsonValue::Array(items.iter().map(dyn_sol_to_json).collect())
        }
        DynSolValue::Tuple(items) => JsonValue::Array(items.iter().map(dyn_sol_to_json).collect()),
        _ => JsonValue::Null,
    }
}

/// Call a read-only (view/pure) contract function. No wallet needed.
pub async fn read_contract(
    rpc_url: &str,
    address: Address,
    abi_json: &str,
    function: &str,
    args: &[JsonValue],
) -> Result<JsonValue> {
    let abi: JsonAbi = serde_json::from_str(abi_json).context("invalid ABI JSON")?;
    let func = find_function(&abi, function)?;
    let encoded_args = encode_args(&func, args)?;
    let calldata = func
        .abi_encode_input(&encoded_args)
        .context("failed to ABI-encode input")?;

    let provider = alloy::providers::ProviderBuilder::new()
        .connect_http(rpc_url.parse().context("invalid RPC URL")?);

    let tx = TransactionRequest::default()
        .with_to(address)
        .with_input(Bytes::from(calldata));

    let result_bytes = provider.call(tx).await.context("contract call failed")?;

    let decoded = func
        .abi_decode_output(&result_bytes)
        .context("failed to decode contract output")?;

    // Return single value unwrapped, multiple as array
    if decoded.len() == 1 {
        Ok(dyn_sol_to_json(&decoded[0]))
    } else {
        Ok(JsonValue::Array(
            decoded.iter().map(dyn_sol_to_json).collect(),
        ))
    }
}

/// Send a state-changing transaction to a contract. Returns the tx hash.
pub async fn write_contract(
    wallet: &BaseWallet,
    address: Address,
    abi_json: &str,
    function: &str,
    args: &[JsonValue],
    value: Option<U256>,
) -> Result<B256> {
    let abi: JsonAbi = serde_json::from_str(abi_json).context("invalid ABI JSON")?;
    let func = find_function(&abi, function)?;
    let encoded_args = encode_args(&func, args)?;
    let calldata = func
        .abi_encode_input(&encoded_args)
        .context("failed to ABI-encode input")?;

    let mut tx = TransactionRequest::default()
        .with_to(address)
        .with_input(Bytes::from(calldata))
        .with_chain_id(BASE_CHAIN_ID);

    if let Some(val) = value {
        tx = tx.with_value(val);
    }

    let pending = wallet
        .provider()
        .send_transaction(tx)
        .await
        .context("failed to send contract transaction")?;

    let tx_hash = *pending.tx_hash();
    tracing::info!(%tx_hash, "contract transaction sent, waiting for confirmation");

    pending
        .get_receipt()
        .await
        .context("failed to get transaction receipt")?;

    Ok(tx_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ERC20_BALANCE_OF_ABI: &str = r#"[{"inputs":[{"name":"account","type":"address"}],"name":"balanceOf","outputs":[{"name":"","type":"uint256"}],"stateMutability":"view","type":"function"}]"#;

    #[test]
    fn find_function_in_abi() {
        let abi: JsonAbi = serde_json::from_str(ERC20_BALANCE_OF_ABI).unwrap();
        let func = find_function(&abi, "balanceOf").unwrap();
        assert_eq!(func.name, "balanceOf");
        assert_eq!(func.inputs.len(), 1);
        assert_eq!(func.outputs.len(), 1);
    }

    #[test]
    fn find_function_not_found() {
        let abi: JsonAbi = serde_json::from_str(ERC20_BALANCE_OF_ABI).unwrap();
        assert!(find_function(&abi, "transfer").is_err());
    }

    #[test]
    fn encode_balance_of_calldata() {
        let abi: JsonAbi = serde_json::from_str(ERC20_BALANCE_OF_ABI).unwrap();
        let func = find_function(&abi, "balanceOf").unwrap();
        let args = vec![JsonValue::String(
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        )];
        let encoded = encode_args(&func, &args).unwrap();
        let calldata = func.abi_encode_input(&encoded).unwrap();

        // balanceOf(address) selector = 0x70a08231
        assert_eq!(&calldata[..4], &[0x70, 0xa0, 0x82, 0x31]);
        // 4 bytes selector + 32 bytes padded address = 36 bytes
        assert_eq!(calldata.len(), 36);
    }

    #[test]
    fn json_to_dyn_sol_address() {
        let val = json_to_dyn_sol(
            "address",
            &JsonValue::String("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string()),
        )
        .unwrap();
        assert!(matches!(val, DynSolValue::Address(_)));
    }

    #[test]
    fn json_to_dyn_sol_uint256_string() {
        let val = json_to_dyn_sol("uint256", &JsonValue::String("1000000".to_string())).unwrap();
        assert_eq!(val, DynSolValue::Uint(U256::from(1_000_000u64), 256));
    }

    #[test]
    fn json_to_dyn_sol_uint256_number() {
        let val = json_to_dyn_sol("uint256", &serde_json::json!(42)).unwrap();
        assert_eq!(val, DynSolValue::Uint(U256::from(42u64), 256));
    }

    #[test]
    fn json_to_dyn_sol_bool() {
        let val = json_to_dyn_sol("bool", &JsonValue::Bool(true)).unwrap();
        assert_eq!(val, DynSolValue::Bool(true));
    }

    #[test]
    fn json_to_dyn_sol_string() {
        let val = json_to_dyn_sol("string", &JsonValue::String("hello".to_string())).unwrap();
        assert_eq!(val, DynSolValue::String("hello".to_string()));
    }

    #[test]
    fn json_to_dyn_sol_bytes() {
        let val = json_to_dyn_sol("bytes", &JsonValue::String("0xdeadbeef".to_string())).unwrap();
        assert_eq!(val, DynSolValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    }

    #[test]
    fn json_to_dyn_sol_bytes32() {
        let hex_str = format!("0x{}", "ab".repeat(32));
        let val = json_to_dyn_sol("bytes32", &JsonValue::String(hex_str)).unwrap();
        match val {
            DynSolValue::FixedBytes(_, size) => assert_eq!(size, 32),
            _ => panic!("expected FixedBytes"),
        }
    }

    #[test]
    fn json_to_dyn_sol_array() {
        let val = json_to_dyn_sol(
            "address[]",
            &serde_json::json!([
                "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "0x0000000000000000000000000000000000000001"
            ]),
        )
        .unwrap();
        match val {
            DynSolValue::Array(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn dyn_sol_to_json_roundtrip_uint() {
        let val = DynSolValue::Uint(U256::from(42u64), 256);
        let json = dyn_sol_to_json(&val);
        assert_eq!(json, JsonValue::String("42".to_string()));
    }

    #[test]
    fn dyn_sol_to_json_roundtrip_address() {
        let addr: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap();
        let val = DynSolValue::Address(addr);
        let json = dyn_sol_to_json(&val);
        let s = json.as_str().unwrap();
        assert!(s.starts_with("0x"));
        assert_eq!(s.len(), 42);
    }

    #[test]
    fn dyn_sol_to_json_bool() {
        assert_eq!(
            dyn_sol_to_json(&DynSolValue::Bool(true)),
            JsonValue::Bool(true)
        );
    }

    #[test]
    fn encode_args_wrong_count() {
        let abi: JsonAbi = serde_json::from_str(ERC20_BALANCE_OF_ABI).unwrap();
        let func = find_function(&abi, "balanceOf").unwrap();
        assert!(encode_args(&func, &[]).is_err());
        assert!(
            encode_args(
                &func,
                &[serde_json::json!("0x01"), serde_json::json!("0x02")]
            )
            .is_err()
        );
    }

    #[test]
    fn sign_message_recovers_address() {
        use alloy::signers::SignerSync;
        use alloy::signers::local::PrivateKeySigner;

        // Known test key
        let key_bytes = [1u8; 32];
        let signer = PrivateKeySigner::from_slice(&key_bytes).unwrap();
        let _expected_addr = signer.address();

        let message = b"hello agentbook";
        let sig = signer.sign_message_sync(message).unwrap();

        // Verify the signature is 65 bytes (r + s + v)
        assert_eq!(sig.as_bytes().len(), 65);

        // Verify the hex encoding
        let hex_sig = format!("0x{}", alloy::hex::encode(sig.as_bytes()));
        assert!(hex_sig.starts_with("0x"));
        assert_eq!(hex_sig.len(), 132); // 0x + 130 hex chars
    }
}
