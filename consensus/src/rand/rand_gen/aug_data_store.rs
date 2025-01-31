// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::rand::rand_gen::{
    storage::interface::AugDataStorage,
    types::{
        AugData, AugDataId, AugDataSignature, AugmentedData, CertifiedAugData, CertifiedAugDataAck,
        RandConfig,
    },
};
use anyhow::ensure;
use aptos_consensus_types::common::Author;
use aptos_logger::error;
use aptos_types::validator_signer::ValidatorSigner;
use std::{collections::HashMap, sync::Arc};

pub struct AugDataStore<D, Storage> {
    epoch: u64,
    signer: Arc<ValidatorSigner>,
    config: RandConfig,
    data: HashMap<Author, AugData<D>>,
    certified_data: HashMap<Author, CertifiedAugData<D>>,
    db: Arc<Storage>,
}

impl<D: AugmentedData, Storage: AugDataStorage<D>> AugDataStore<D, Storage> {
    fn filter_by_epoch<T>(
        epoch: u64,
        all_data: impl Iterator<Item = (AugDataId, T)>,
    ) -> (Vec<T>, Vec<(AugDataId, T)>) {
        let mut to_remove = vec![];
        let mut to_keep = vec![];
        for (id, data) in all_data {
            if id.epoch() != epoch {
                to_remove.push(data)
            } else {
                to_keep.push((id, data))
            }
        }
        (to_remove, to_keep)
    }

    pub fn new(
        epoch: u64,
        signer: Arc<ValidatorSigner>,
        config: RandConfig,
        db: Arc<Storage>,
    ) -> Self {
        let all_data = db.get_all_aug_data().unwrap_or_default();
        let (to_remove, aug_data) = Self::filter_by_epoch(epoch, all_data.into_iter());
        if let Err(e) = db.remove_aug_data(to_remove.into_iter()) {
            error!("[AugDataStore] failed to remove aug data: {:?}", e);
        }

        let all_certified_data = db.get_all_certified_aug_data().unwrap_or_default();
        let (to_remove, certified_data) =
            Self::filter_by_epoch(epoch, all_certified_data.into_iter());
        if let Err(e) = db.remove_certified_aug_data(to_remove.into_iter()) {
            error!(
                "[AugDataStore] failed to remove certified aug data: {:?}",
                e
            );
        }

        Self {
            epoch,
            signer,
            config,
            data: aug_data
                .into_iter()
                .map(|(id, data)| (id.author(), data))
                .collect(),
            certified_data: certified_data
                .into_iter()
                .map(|(id, data)| (id.author(), data))
                .collect(),
            db,
        }
    }

    pub fn add_aug_data(&mut self, data: AugData<D>) -> anyhow::Result<AugDataSignature> {
        if let Some(existing_data) = self.data.get(&data.author()) {
            ensure!(
                existing_data == &data,
                "[AugDataStore] equivocate data from {}",
                data.author()
            );
        } else {
            self.db.save_aug_data(&data)?;
        }
        let sig = AugDataSignature::new(self.epoch, self.signer.sign(&data)?);
        self.data.insert(data.author(), data);
        Ok(sig)
    }

    pub fn add_certified_aug_data(
        &mut self,
        data: CertifiedAugData<D>,
    ) -> anyhow::Result<CertifiedAugDataAck> {
        if self.certified_data.contains_key(&data.author()) {
            return Ok(CertifiedAugDataAck::new(self.epoch));
        }
        self.db.save_certified_aug_data(&data)?;
        self.certified_data.insert(data.author(), data);
        Ok(CertifiedAugDataAck::new(self.epoch))
    }
}
