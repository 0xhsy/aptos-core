spec aptos_framework::managed_coin {
    spec module {
        pragma verify = true;
        pragma aborts_if_is_strict;
    }

    spec burn<CoinType>(
    account: &signer,
    amount: u64,
    ) {
        pragma verify = false;
    }

    /// Make sure `name` and `symbol` are legal length.
    /// Only the creator of `CoinType` can initialize.
    /// The 'name' and 'symbol' should be valid utf8 bytes
    /// The Capabilities<CoinType> should not be under the signer before creating;
    /// The Capabilities<CoinType> should be under the signer after creating;
    spec initialize<CoinType>(
    account: &signer,
    name: vector<u8>,
    symbol: vector<u8>,
    decimals: u8,
    monitor_supply: bool,
    ) {
        include coin::InitializeInternalSchema<CoinType>;
        aborts_if !string::spec_internal_check_utf8(name);
        aborts_if !string::spec_internal_check_utf8(symbol);
        aborts_if exists<Capabilities<CoinType>>(signer::address_of(account));
        ensures exists<Capabilities<CoinType>>(signer::address_of(account));
    }

    /// The Capabilities<CoinType> should not exist in the signer address.
    /// The `dst_addr` should not be frozen.
    spec mint<CoinType>(
    account: &signer,
    dst_addr: address,
    amount: u64,
    ) {
        pragma verify = false;
    }

    /// An account can only be registered once.
    /// Updating `Account.guid_creation_num` will not overflow.
    spec register<CoinType>(account: &signer) {}
}
