use super::{
    Command, Compounded, DynSubscription, MutateSingle, Mutation, MutationOutcome, MutationPolicies, UtxosChangedMutationPolicy,
    compounded,
    context::SubscriptionContext,
    single::{UtxosChangedState, UtxosChangedSubscription},
};
use crate::{
    events::EventType,
    scope::{Scope, UtxosChangedFilteredScope, UtxosChangedScope},
};
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_hashes::Hash;
use std::sync::Arc;

#[test]
fn address_mutations_preserve_prefixes_with_and_without_covenant_subscriptions() {
    for prefix in [Prefix::Testnet, Prefix::Simnet] {
        for with_covenant in [false, true] {
            let context = SubscriptionContext::new();
            let mut subscription = listener(1);
            let mut parent = compounded::UtxosChangedSubscription::new();
            if with_covenant {
                for mutation in
                    apply(&mut subscription, Command::Start, vec![Hash::from_u64_word(1)], MutationPolicies::default(), &context)
                        .mutations
                {
                    parent.compound(mutation, &context);
                }
            }
            let address = Address::new(prefix, Version::PubKey, &[1; 32]);
            for command in [Command::Start, Command::Stop] {
                let mutation = Mutation::new(command, UtxosChangedScope::new(vec![address.clone()]).into());
                let outcome = subscription.mutate(mutation.clone(), MutationPolicies::default(), &context).unwrap();
                assert_eq!(outcome.mutations, vec![mutation.clone()]);
                assert_eq!(parent.compound(outcome.mutations.into_iter().next().unwrap(), &context), Some(mutation));
            }
        }
    }
}

#[test]
fn covenant_subscriptions_coexist_with_addresses_and_unsubscribe_independently() {
    let context = SubscriptionContext::new();
    let address = Address::new(Prefix::Mainnet, Version::PubKey, &[1; 32]);
    let id = Hash::from_u64_word(1);
    let mut subscription = listener(1);
    let policies = MutationPolicies::default();
    subscription
        .mutate(Mutation::new(Command::Start, UtxosChangedScope::new(vec![address.clone()]).into()), policies, &context)
        .unwrap();
    let added = apply(&mut subscription, Command::Start, vec![id, id], policies, &context);
    assert_eq!(added.mutations, vec![covenants(Command::Start, vec![id])]);
    assert!(!apply(&mut subscription, Command::Start, vec![id], policies, &context).has_changes());
    let both = UtxosChangedFilteredScope::new(vec![address.clone()], vec![id]);
    assert_eq!(subscription.scope(&context), Scope::from(both));

    apply(&mut subscription, Command::Stop, vec![id], policies, &context);
    assert_eq!(subscription.scope(&context), UtxosChangedScope::new(vec![address.clone()]).into());
    apply(&mut subscription, Command::Start, vec![id], policies, &context);
    subscription.mutate(Mutation::new(Command::Stop, UtxosChangedScope::new(vec![address]).into()), policies, &context).unwrap();
    assert_eq!(subscription.scope(&context), covenants(Command::Start, vec![id]).scope);
    subscription.mutate(Mutation::new(Command::Stop, UtxosChangedScope::default().into()), policies, &context).unwrap();
    assert!(!subscription.active());
}

#[test]
fn empty_covenant_filters_never_become_blanket_subscriptions() {
    let mut subscription = listener(1);
    for command in [Command::Start, Command::Stop] {
        let result = subscription.mutate(covenants(command, vec![]), MutationPolicies::default(), &SubscriptionContext::new());
        assert!(matches!(result, Err(crate::error::Error::EmptyUtxosChangedFilter)));
        assert!(!subscription.active());
    }
}

#[test]
fn mixed_subscription_capacity_failure_does_not_leave_partial_registration() {
    let context = SubscriptionContext::with_options(Some(6));
    let addresses: Vec<_> = (0..5).map(|i| Address::new(Prefix::Mainnet, Version::PubKey, &[i; 32])).collect();
    let mut holder = listener(1);
    holder
        .mutate(
            Mutation::new(Command::Start, UtxosChangedFilteredScope::new(addresses.clone(), vec![Hash::from_u64_word(1)]).into()),
            MutationPolicies::default(),
            &context,
        )
        .unwrap();
    let mut other = listener(2);
    let result = other.mutate(
        Mutation::new(Command::Start, UtxosChangedFilteredScope::new(vec![addresses[0].clone()], vec![Hash::from_u64_word(2)]).into()),
        MutationPolicies::default(),
        &context,
    );
    assert!(matches!(result, Err(crate::error::Error::AddressError(crate::address::error::Error::MaxCapacityReached))));
    assert!(!other.active());
    assert!(!other.as_any().downcast_ref::<UtxosChangedSubscription>().unwrap().data().has_covenant_ids());
    assert_eq!(context.address_tracker.get_address(&addresses[0]).unwrap().1, 1);
    assert_eq!(context.address_tracker.len(), 6);
}

#[test]
fn covenant_subscriptions_reduce_to_wildcard_only_at_wildcard_parents() {
    let context = SubscriptionContext::new();
    let mut subscription = listener(1);
    let policies = MutationPolicies::new(UtxosChangedMutationPolicy::Wildcard);
    let a = Hash::from_u64_word(1);
    let b = Hash::from_u64_word(2);
    let all = |command| Mutation::new(command, UtxosChangedScope::default().into());
    assert_eq!(apply(&mut subscription, Command::Start, vec![a], policies, &context).mutations, vec![all(Command::Start)]);
    assert!(apply(&mut subscription, Command::Start, vec![b], policies, &context).mutations.is_empty());
    assert!(apply(&mut subscription, Command::Stop, vec![a], policies, &context).mutations.is_empty());
    assert_eq!(apply(&mut subscription, Command::Stop, vec![b], policies, &context).mutations, vec![all(Command::Stop)]);
    assert!(!subscription.active());
}

#[test]
fn covenant_subscriptions_switch_between_selected_and_all() {
    for policy in [UtxosChangedMutationPolicy::AddressSet, UtxosChangedMutationPolicy::Wildcard] {
        let context = SubscriptionContext::new();
        let mut subscription = listener(1);
        let policies = MutationPolicies::new(policy);
        let selected = |command| covenants(command, vec![Hash::from_u64_word(1)]);
        let all = |command| Mutation::new(command, UtxosChangedScope::default().into());
        subscription.mutate(selected(Command::Start), policies, &context).unwrap();

        let outcome = subscription.mutate(all(Command::Start), policies, &context).unwrap();
        let expected = match policy {
            UtxosChangedMutationPolicy::AddressSet => vec![selected(Command::Stop), all(Command::Start)],
            UtxosChangedMutationPolicy::Wildcard => vec![],
        };
        assert_eq!(outcome.mutations, expected);
        assert_eq!(subscription.scope(&context), all(Command::Start).scope);
        assert!(context.address_tracker.is_empty());

        let outcome = subscription.mutate(selected(Command::Start), policies, &context).unwrap();
        let expected = match policy {
            UtxosChangedMutationPolicy::AddressSet => vec![selected(Command::Start), all(Command::Stop)],
            UtxosChangedMutationPolicy::Wildcard => vec![],
        };
        assert_eq!(outcome.mutations, expected);
        assert_eq!(subscription.scope(&context), selected(Command::Start).scope);
        assert_eq!(context.address_tracker.len(), 1);
        subscription.mutate(all(Command::Stop), policies, &context).unwrap();
        assert!(!subscription.active());
        assert!(context.address_tracker.is_empty());
    }
}

#[test]
fn covenant_reference_counts_survive_shared_subscriptions_and_blanket_masks() {
    let context = SubscriptionContext::new();
    let mut parent = compounded::UtxosChangedSubscription::new();
    let [mut first, mut second, mut blanket] = [listener(1), listener(2), listener(3)];
    let a = Hash::from_u64_word(1);
    let b = Hash::from_u64_word(2);
    let mut forward = |listener: &mut DynSubscription, mutation: Mutation| {
        listener
            .mutate(mutation, MutationPolicies::default(), &context)
            .unwrap()
            .mutations
            .into_iter()
            .filter_map(|mutation| parent.compound(mutation, &context))
            .collect::<Vec<_>>()
    };
    assert_eq!(forward(&mut first, covenants(Command::Start, vec![a])), vec![covenants(Command::Start, vec![a])]);
    assert!(forward(&mut second, covenants(Command::Start, vec![a])).is_empty());
    assert!(forward(&mut first, covenants(Command::Stop, vec![a])).is_empty());
    let all = |command| Mutation::new(command, UtxosChangedScope::default().into());
    assert_eq!(forward(&mut blanket, all(Command::Start)), vec![all(Command::Start)]);
    assert!(forward(&mut second, covenants(Command::Start, vec![b])).is_empty());
    assert!(forward(&mut second, covenants(Command::Stop, vec![a])).is_empty());
    assert_eq!(forward(&mut blanket, all(Command::Stop)), vec![covenants(Command::Start, vec![b])]);
    assert_eq!(forward(&mut second, covenants(Command::Stop, vec![b])), vec![covenants(Command::Stop, vec![b])]);
    assert!(context.address_tracker.is_empty());
}

#[test]
fn covenant_scope_preserves_legacy_encoding_and_uses_the_existing_event() {
    let addresses = vec![Address::new(Prefix::Mainnet, Version::PubKey, &[1; 32])];
    let mut legacy_encoding = vec![4]; // Existing UtxosChanged discriminant.
    borsh::BorshSerialize::serialize(&addresses, &mut legacy_encoding).unwrap();
    assert_eq!(borsh::to_vec(&Scope::from(UtxosChangedScope::new(addresses.clone()))).unwrap(), legacy_encoding);
    let scope = Scope::from(UtxosChangedFilteredScope::new(addresses, vec![Hash::from_u64_word(1)]));
    let encoded = borsh::to_vec(&scope).unwrap();
    assert_eq!(encoded[0], 9); // Appended; all existing scope discriminants remain unchanged.
    assert_eq!(borsh::from_slice::<Scope>(&encoded).unwrap(), scope);
    assert_eq!(scope.event_type(), EventType::UtxosChanged);
}

fn listener(id: u64) -> DynSubscription {
    Arc::new(UtxosChangedSubscription::new(UtxosChangedState::None, id))
}

fn covenants(command: Command, ids: Vec<Hash>) -> Mutation {
    Mutation::new(command, UtxosChangedFilteredScope::new(vec![], ids).into())
}

fn apply(
    subscription: &mut DynSubscription,
    command: Command,
    ids: Vec<Hash>,
    policies: MutationPolicies,
    context: &SubscriptionContext,
) -> MutationOutcome {
    subscription.mutate(covenants(command, ids), policies, context).unwrap()
}
