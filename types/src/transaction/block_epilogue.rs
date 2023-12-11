// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use aptos_crypto::HashValue;
#[cfg(any(test, feature = "fuzzing"))]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "fuzzing"), derive(Arbitrary))]
pub enum BlockEpiloguePayload {
    BlockId(HashValue),
    WithBlockEndInfo {
        block_id: HashValue,
        block_end_info: BlockEndInfo,
    },
}

impl BlockEpiloguePayload {
    pub fn try_as_block_end_info(&self) -> Option<&BlockEndInfo> {
        match self {
            BlockEpiloguePayload::BlockId(_) => None,
            BlockEpiloguePayload::WithBlockEndInfo { block_end_info, .. } => Some(block_end_info),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockEndInfo {
    /// Whether block gas limit was reached
    pub block_gas_limit_reached: bool,
    /// Whether block output limit was reached
    pub block_output_limit_reached: bool,

    pub block_effective_block_gas: u64,

    pub block_approx_output_size: u64,
}
