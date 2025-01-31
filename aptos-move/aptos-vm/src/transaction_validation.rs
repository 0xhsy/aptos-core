// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    errors::{
        convert_epilogue_error, convert_prologue_error, discarded_output,
        expect_only_successful_execution,
    },
    move_vm_ext::SessionExt,
    system_module_names::{
        EMIT_FEE_STATEMENT, MULTISIG_ACCOUNT_MODULE, TRANSACTION_FEE_MODULE,
        VALIDATE_MULTISIG_TRANSACTION,
    },
    testing::{maybe_raise_injected_error, InjectedError},
    transaction_metadata::TransactionMetadata,
};
use aptos_gas_algebra::Gas;
use aptos_types::{
    account_config::constants::CORE_CODE_ADDRESS, fee_statement::FeeStatement,
    on_chain_config::Features, transaction::Multisig,
};
use aptos_vm_logging::log_schema::AdapterLogSchema;
use aptos_vm_types::output::VMOutput;
use fail::fail_point;
use move_binary_format::errors::VMResult;
use move_core_types::{
    account_address::AccountAddress,
    ident_str,
    identifier::Identifier,
    language_storage::ModuleId,
    value::{serialize_values, MoveValue},
    vm_status::{AbortLocation, StatusCode, VMStatus},
};
use move_vm_runtime::logging::expect_no_verification_errors;
use move_vm_types::gas::UnmeteredGasMeter;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

pub static APTOS_TRANSACTION_VALIDATION: Lazy<TransactionValidation> =
    Lazy::new(|| TransactionValidation {
        module_addr: CORE_CODE_ADDRESS,
        module_name: Identifier::new("transaction_validation").unwrap(),
        fee_payer_prologue_name: Identifier::new("fee_payer_script_prologue").unwrap(),
        script_prologue_name: Identifier::new("script_prologue").unwrap(),
        module_prologue_name: Identifier::new("module_prologue").unwrap(),
        multi_agent_prologue_name: Identifier::new("multi_agent_script_prologue").unwrap(),
        user_epilogue_name: Identifier::new("epilogue").unwrap(),
        user_epilogue_gas_payer_name: Identifier::new("epilogue_gas_payer").unwrap(),
    });

/// On-chain functions used to validate transactions
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionValidation {
    pub module_addr: AccountAddress,
    pub module_name: Identifier,
    pub fee_payer_prologue_name: Identifier,
    pub script_prologue_name: Identifier,
    pub module_prologue_name: Identifier,
    pub multi_agent_prologue_name: Identifier,
    pub user_epilogue_name: Identifier,
    pub user_epilogue_gas_payer_name: Identifier,
}

impl TransactionValidation {
    pub fn module_id(&self) -> ModuleId {
        ModuleId::new(self.module_addr, self.module_name.clone())
    }

    pub fn is_account_module_abort(&self, location: &AbortLocation) -> bool {
        location == &AbortLocation::Module(self.module_id())
            || location
                == &AbortLocation::Module(ModuleId::new(
                    CORE_CODE_ADDRESS,
                    ident_str!("transaction_validation").to_owned(),
                ))
    }
}

pub(crate) fn run_script_prologue(
    session: &mut SessionExt,
    txn_data: &TransactionMetadata,
    log_context: &AdapterLogSchema,
) -> Result<(), VMStatus> {
    let txn_sequence_number = txn_data.sequence_number();
    let txn_authentication_key = txn_data.authentication_key().to_vec();
    let txn_gas_price = txn_data.gas_unit_price();
    let txn_max_gas_units = txn_data.max_gas_amount();
    let txn_expiration_timestamp_secs = txn_data.expiration_timestamp_secs();
    let chain_id = txn_data.chain_id();
    let mut gas_meter = UnmeteredGasMeter;
    let secondary_auth_keys: Vec<MoveValue> = txn_data
        .secondary_authentication_keys
        .iter()
        .map(|auth_key| MoveValue::vector_u8(auth_key.to_vec()))
        .collect();

    let (prologue_function_name, args) = if let (Some(fee_payer), Some(fee_payer_auth_key)) = (
        txn_data.fee_payer(),
        txn_data.fee_payer_authentication_key.as_ref(),
    ) {
        let args = vec![
            MoveValue::Signer(txn_data.sender),
            MoveValue::U64(txn_sequence_number),
            MoveValue::vector_u8(txn_authentication_key),
            MoveValue::vector_address(txn_data.secondary_signers()),
            MoveValue::Vector(secondary_auth_keys),
            MoveValue::Address(fee_payer),
            MoveValue::vector_u8(fee_payer_auth_key.to_vec()),
            MoveValue::U64(txn_gas_price.into()),
            MoveValue::U64(txn_max_gas_units.into()),
            MoveValue::U64(txn_expiration_timestamp_secs),
            MoveValue::U8(chain_id.id()),
        ];
        (&APTOS_TRANSACTION_VALIDATION.fee_payer_prologue_name, args)
    } else if txn_data.is_multi_agent() {
        let args = vec![
            MoveValue::Signer(txn_data.sender),
            MoveValue::U64(txn_sequence_number),
            MoveValue::vector_u8(txn_authentication_key),
            MoveValue::vector_address(txn_data.secondary_signers()),
            MoveValue::Vector(secondary_auth_keys),
            MoveValue::U64(txn_gas_price.into()),
            MoveValue::U64(txn_max_gas_units.into()),
            MoveValue::U64(txn_expiration_timestamp_secs),
            MoveValue::U8(chain_id.id()),
        ];
        (
            &APTOS_TRANSACTION_VALIDATION.multi_agent_prologue_name,
            args,
        )
    } else {
        let args = vec![
            MoveValue::Signer(txn_data.sender),
            MoveValue::U64(txn_sequence_number),
            MoveValue::vector_u8(txn_authentication_key),
            MoveValue::U64(txn_gas_price.into()),
            MoveValue::U64(txn_max_gas_units.into()),
            MoveValue::U64(txn_expiration_timestamp_secs),
            MoveValue::U8(chain_id.id()),
            MoveValue::vector_u8(txn_data.script_hash.clone()),
        ];
        (&APTOS_TRANSACTION_VALIDATION.script_prologue_name, args)
    };
    session
        .execute_function_bypass_visibility(
            &APTOS_TRANSACTION_VALIDATION.module_id(),
            prologue_function_name,
            vec![],
            serialize_values(&args),
            &mut gas_meter,
        )
        .map(|_return_vals| ())
        .map_err(expect_no_verification_errors)
        .or_else(|err| convert_prologue_error(err, log_context))
}

// Deprecated (see ModuleBundle).
pub(crate) fn run_module_prologue(
    session: &mut SessionExt,
    txn_data: &TransactionMetadata,
    log_context: &AdapterLogSchema,
) -> Result<(), VMStatus> {
    let txn_sequence_number = txn_data.sequence_number();
    let txn_authentication_key = txn_data.authentication_key();
    let txn_gas_price = txn_data.gas_unit_price();
    let txn_max_gas_units = txn_data.max_gas_amount();
    let txn_expiration_timestamp_secs = txn_data.expiration_timestamp_secs();
    let chain_id = txn_data.chain_id();
    let mut gas_meter = UnmeteredGasMeter;
    session
        .execute_function_bypass_visibility(
            &APTOS_TRANSACTION_VALIDATION.module_id(),
            &APTOS_TRANSACTION_VALIDATION.module_prologue_name,
            vec![],
            serialize_values(&vec![
                MoveValue::Signer(txn_data.sender),
                MoveValue::U64(txn_sequence_number),
                MoveValue::vector_u8(txn_authentication_key.to_vec()),
                MoveValue::U64(txn_gas_price.into()),
                MoveValue::U64(txn_max_gas_units.into()),
                MoveValue::U64(txn_expiration_timestamp_secs),
                MoveValue::U8(chain_id.id()),
            ]),
            &mut gas_meter,
        )
        .map(|_return_vals| ())
        .map_err(expect_no_verification_errors)
        .or_else(|err| convert_prologue_error(err, log_context))
}

/// Run the prologue for a multisig transaction. This needs to verify that:
/// 1. The the multisig tx exists
/// 2. It has received enough approvals to meet the signature threshold of the multisig account
/// 3. If only the payload hash was stored on chain, the provided payload in execution should
/// match that hash.
pub(crate) fn run_multisig_prologue(
    session: &mut SessionExt,
    txn_data: &TransactionMetadata,
    payload: &Multisig,
    log_context: &AdapterLogSchema,
) -> Result<(), VMStatus> {
    let unreachable_error = VMStatus::error(StatusCode::UNREACHABLE, None);
    let provided_payload = if let Some(payload) = &payload.transaction_payload {
        bcs::to_bytes(&payload).map_err(|_| unreachable_error.clone())?
    } else {
        // Default to empty bytes if payload is not provided.
        bcs::to_bytes::<Vec<u8>>(&vec![]).map_err(|_| unreachable_error)?
    };

    session
        .execute_function_bypass_visibility(
            &MULTISIG_ACCOUNT_MODULE,
            VALIDATE_MULTISIG_TRANSACTION,
            vec![],
            serialize_values(&vec![
                MoveValue::Signer(txn_data.sender),
                MoveValue::Address(payload.multisig_address),
                MoveValue::vector_u8(provided_payload),
            ]),
            &mut UnmeteredGasMeter,
        )
        .map(|_return_vals| ())
        .map_err(expect_no_verification_errors)
        .or_else(|err| convert_prologue_error(err, log_context))
}

fn run_epilogue(
    session: &mut SessionExt,
    gas_remaining: Gas,
    fee_statement: FeeStatement,
    txn_data: &TransactionMetadata,
    features: &Features,
) -> VMResult<()> {
    let txn_gas_price = txn_data.gas_unit_price();
    let txn_max_gas_units = txn_data.max_gas_amount();

    // We can unconditionally do this as this condition can only be true if the prologue
    // accepted it, in which case the gas payer feature is enabled.
    if let Some(fee_payer) = txn_data.fee_payer() {
        session.execute_function_bypass_visibility(
            &APTOS_TRANSACTION_VALIDATION.module_id(),
            &APTOS_TRANSACTION_VALIDATION.user_epilogue_gas_payer_name,
            vec![],
            serialize_values(&vec![
                MoveValue::Signer(txn_data.sender),
                MoveValue::Address(fee_payer),
                MoveValue::U64(fee_statement.storage_fee_refund()),
                MoveValue::U64(txn_gas_price.into()),
                MoveValue::U64(txn_max_gas_units.into()),
                MoveValue::U64(gas_remaining.into()),
            ]),
            &mut UnmeteredGasMeter,
        )
    } else {
        // Regular tx, run the normal epilogue
        session.execute_function_bypass_visibility(
            &APTOS_TRANSACTION_VALIDATION.module_id(),
            &APTOS_TRANSACTION_VALIDATION.user_epilogue_name,
            vec![],
            serialize_values(&vec![
                MoveValue::Signer(txn_data.sender),
                MoveValue::U64(fee_statement.storage_fee_refund()),
                MoveValue::U64(txn_gas_price.into()),
                MoveValue::U64(txn_max_gas_units.into()),
                MoveValue::U64(gas_remaining.into()),
            ]),
            &mut UnmeteredGasMeter,
        )
    }
    .map(|_return_vals| ())
    .map_err(expect_no_verification_errors)?;

    // Emit the FeeStatement event
    if features.is_emit_fee_statement_enabled() {
        emit_fee_statement(session, fee_statement)?;
    }

    maybe_raise_injected_error(InjectedError::EndOfRunEpilogue)?;

    Ok(())
}

fn emit_fee_statement(session: &mut SessionExt, fee_statement: FeeStatement) -> VMResult<()> {
    session
        .execute_function_bypass_visibility(
            &TRANSACTION_FEE_MODULE,
            EMIT_FEE_STATEMENT,
            vec![],
            vec![bcs::to_bytes(&fee_statement).expect("Failed to serialize fee statement")],
            &mut UnmeteredGasMeter,
        )
        .map(|_return_vals| ())
}

/// Run the epilogue of a transaction by calling into `EPILOGUE_NAME` function stored
/// in the `ACCOUNT_MODULE` on chain.
pub(crate) fn run_success_epilogue(
    session: &mut SessionExt,
    gas_remaining: Gas,
    fee_statement: FeeStatement,
    features: &Features,
    txn_data: &TransactionMetadata,
    log_context: &AdapterLogSchema,
) -> Result<(), VMStatus> {
    fail_point!("move_adapter::run_success_epilogue", |_| {
        Err(VMStatus::error(
            StatusCode::UNKNOWN_INVARIANT_VIOLATION_ERROR,
            None,
        ))
    });

    run_epilogue(session, gas_remaining, fee_statement, txn_data, features)
        .or_else(|err| convert_epilogue_error(err, log_context))
}

/// Run the failure epilogue of a transaction by calling into `USER_EPILOGUE_NAME` function
/// stored in the `ACCOUNT_MODULE` on chain.
pub(crate) fn run_failure_epilogue(
    session: &mut SessionExt,
    gas_remaining: Gas,
    fee_statement: FeeStatement,
    features: &Features,
    txn_data: &TransactionMetadata,
    log_context: &AdapterLogSchema,
) -> Option<(VMStatus, VMOutput)> {
    if let Err(e) =
        run_epilogue(session, gas_remaining, fee_statement, txn_data, features).or_else(|e| {
            expect_only_successful_execution(
                e,
                APTOS_TRANSACTION_VALIDATION.user_epilogue_name.as_str(),
                log_context,
            )
        })
    {
        let discarded_output = discarded_output(e.status_code());
        Some((e, discarded_output))
    } else {
        None
    }
}
