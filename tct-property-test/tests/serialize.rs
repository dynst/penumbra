#![recursion_limit = "256"]

#[macro_use]
extern crate proptest_derive;

use std::fmt::{Debug, Display};

use proptest::{arbitrary::*, prelude::*};

use penumbra_tct::{
    storage::{deserialize, serialize, InMemory},
    validate, Commitment, Forgotten, Tree, Witness,
};

const MAX_USED_COMMITMENTS: usize = 3;
const MAX_TIER_ACTIONS: usize = 10;

#[derive(Debug, Copy, Clone, Arbitrary)]
#[proptest(params("Vec<Commitment>"))]
enum Action {
    Insert(Witness, Commitment),
    EndBlock,
    EndEpoch,
    Forget(Commitment),
    Serialize,
}

#[derive(Debug, Clone, Default)]
struct State {
    last_forgotten: Forgotten,
    storage: InMemory,
}

impl Action {
    async fn apply(&self, state: &mut State, tree: &mut Tree) -> anyhow::Result<()> {
        match self {
            Action::Insert(witness, commitment) => {
                tree.insert(*witness, *commitment)?;
            }
            Action::EndBlock => {
                tree.end_block()?;
            }
            Action::EndEpoch => {
                tree.end_epoch()?;
            }
            Action::Forget(commitment) => {
                tree.forget(*commitment);
            }
            Action::Serialize => {
                serialize::to_writer(state.last_forgotten, &mut state.storage, tree).await?;

                state.last_forgotten = tree.forgotten();
            }
        };

        Ok(())
    }
}

proptest! {
    #[test]
    fn incremental_serialize(
        actions in
            prop::collection::vec(any::<Commitment>(), 1..MAX_USED_COMMITMENTS)
                .prop_flat_map(|commitments| {
                    prop::collection::vec(any_with::<Action>(commitments), 1..MAX_TIER_ACTIONS)
                })
                .prop_map(|mut actions| {
                    // Ensure that every sequence of actions ends in a serialization
                    actions.push(Action::Serialize);
                    actions
                })
    ) {
        futures::executor::block_on(async move {
            let mut tree = Tree::new();
            let mut state = State::default();

            // Run all the actions in sequence
            for action in actions {
                action.apply(&mut state, &mut tree).await.unwrap();
            }

            // Make a new copy of the tree by deserializing from the storage
            let deserialized = deserialize::from_reader(&mut state.storage).await.unwrap();

           // After running all the actions, the deserialization of the stored tree should match
            // our in-memory tree (this only holds because we ensured that the last action is always
            // a `Serialize`)
            assert_eq!(tree, deserialized, "mismatch when deserializing from storage: {:?}", state.storage);

            // It should also hold that the result of any sequence of incremental serialization is
            // the same as merely serializing the result all at once, after the fact
            let mut non_incremental = InMemory::default();

            // To check this, we first serialize to a new in-memory storage instance
            serialize::to_writer(
                Forgotten::default(),
                &mut non_incremental,
                &deserialized,
            )
            .await
            .unwrap();

            // Then we check both that the storage matches the incrementally-built one
            assert_eq!(state.storage, non_incremental, "incremental storage mismatches non-incremental storage");

            // Higher-order helper function to factor out common behavior of validation assertions
            fn v<E: Display + Debug + 'static>(validate: fn(&Tree) -> Result<(), E>) -> Box<dyn Fn(&Tree, &InMemory)> {
                Box::new(move |deserialized, storage| if let Err(error) = validate(deserialized) {
                    panic!("{error}:\n\nERROR: {error:?}\n\nDESERIALIZED: {deserialized:?}\n\nSTORAGE: {:?}", storage);
                })
            }

             // Validate the internal structure of the deserialized tree
            for validate in [
                v(validate::index),
                v(validate::all_proofs),
                v(validate::cached_hashes),
                v(validate::forgotten)
            ] {
                validate(&deserialized, &state.storage);
            }
        })
    }
}
