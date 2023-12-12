//# init --private-keys Alice=56a26140eb233750cd14fb168c3eb4bd0782b099cde626ec8aff7f3cceb6364f

//# publish --print-bytecode
module 0x42::game {

    // #[test_only]
    use aptos_std::debug;
    // #[test_only]
    use std::signer;
    // #[test_only]
    use std::vector;

    struct InnerStruct has key, store, copy {
        amount: u64
    }

    struct OuterStruct has key {
        any_field: vector<InnerStruct>
    }

    // #[test(account=@0x1234)]
    public entry fun test_upgrade(account: &signer) acquires OuterStruct {
        let account_address = signer::address_of(account);
        // let upgrade_amount = 0;
        move_to(account, OuterStruct {any_field: vector::empty()});
        let anystruct = borrow_global_mut<OuterStruct>(account_address);
        vector::for_each_mut<InnerStruct>(&mut anystruct.any_field, |field| {
            debug::print(field); // INTERNAL TEST ERROR: INTERNAL VM INVARIANT VIOLATION
            // debug::print(b"foo"); // INTERNAL TEST ERROR: INTERNAL VM INVARIANT VIOLATION
            // let field: &mut InnerStruct = field;
            // field.amount = field.amount + upgrade_amount;
        });
    }
}

//# run 0x42::game::test_upgrade --signers 0x1234 --private-key Alice
