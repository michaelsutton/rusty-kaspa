mod error;
use ark_bn254::Bn254;
use ark_groth16::{Groth16, Proof, VerifyingKey};
use ark_serialize::CanonicalDeserialize;
use kaspa_consensus_core::mass::ScriptUnits;

pub use error::Groth16Error;

use crate::{
    EngineFlags,
    data_stack::Stack,
    opcodes::i32s_to_usizes,
    runtime_resource_meter::RuntimeResourceMeter,
    zk_precompiles::{
        ZkPrecompile,
        fields::{Fr, TruncFr},
    },
};

/// Byte offset of the gamma_abc_g1 length prefix inside a compressed BN254
/// It consists of: alpha_g1 (32 bytes) + beta_g2 (64 bytes) + gamma_g2 (64 bytes) + delta_g2 (64 bytes)
const VK_FIXED_PREFIX_LEN: usize = 32 + 64 * 3;

/// Width of ark-serialize's Vec length prefix
const GAMMA_ABC_G1_LEN_PREFIX_BYTES: usize = 8;

/// Empirically determined script unit cost per gamma_abc_g1 element in the VK
/// such that the total verification cost is within 10ms.
pub const GROTH16_GAMMA_ABC_G1_ELEMENT_SCRIPT_UNITS: u64 = 60_000;

pub struct Groth16Precompile;
impl ZkPrecompile for Groth16Precompile {
    type Error = Groth16Error;
    /// Verifies the integrity of a Groth16 proof.
    ///
    /// *NOTE: Experimental code; not yet fully audited for mainnet use.* TODO(pre-covpp)
    fn verify_zk(dstack: &mut Stack, meter: &mut RuntimeResourceMeter, flags: EngineFlags) -> Result<(), Self::Error> {
        let [unprepared_compressed_key] = dstack.pop_raw()?;

        // Retrieve compressed proof
        let [proof_bytes] = dstack.pop_raw()?;

        // Retrieve number of public inputs
        let [n_inputs] = i32s_to_usizes(dstack.pop_items::<1, i32>()?)?;

        // Retrieve public inputs
        let mut unprepared_public_inputs = Vec::with_capacity(n_inputs);

        // For each public input, pop from the stack and convert to Fr.
        //
        // Note: public input count is bounded by the script stack depth limit.
        for _ in 0..n_inputs {
            // convert bytes to Fr according to whether we're in hardened mode or not
            let fr = if flags.zk_hardening_enabled {
                let [fr] = dstack.pop_items::<1, Fr>()?;
                fr
            } else {
                let [trunc_fr] = dstack.pop_items::<1, TruncFr>()?;
                Fr::from(trunc_fr)
            };
            unprepared_public_inputs.push(fr);
        }

        if flags.zk_hardening_enabled {
            // Charge per gamma_abc_g1 element before deserialization.
            let len_bytes: [u8; GAMMA_ABC_G1_LEN_PREFIX_BYTES] = unprepared_compressed_key
                .get(VK_FIXED_PREFIX_LEN..VK_FIXED_PREFIX_LEN + GAMMA_ABC_G1_LEN_PREFIX_BYTES)
                .and_then(|s| s.try_into().ok())
                .ok_or(Groth16Error::MalformedVerifyingKey)?;
            let gamma_abc_element_count = u64::from_le_bytes(len_bytes);

            // Covered by the arity check below but kept for a clearer error.
            if gamma_abc_element_count == 0 {
                return Err(Groth16Error::EmptyGammaAbc);
            }
            // Public inputs are stack-depth bounded, so +1 cannot overflow.
            if unprepared_public_inputs.len() as u64 + 1 != gamma_abc_element_count {
                return Err(ark_relations::gr1cs::SynthesisError::ArityMismatch.into());
            }

            let gamma_abc_cost = ScriptUnits(gamma_abc_element_count.saturating_mul(GROTH16_GAMMA_ABC_G1_ELEMENT_SCRIPT_UNITS));
            meter.consume_script_units(gamma_abc_cost)?;
        }

        let vk = VerifyingKey::deserialize_compressed(&*unprepared_compressed_key)?;

        // Over-defensive double check that the deserialized vk has the expected gamma_abc_g1 count.
        if flags.zk_hardening_enabled && (unprepared_public_inputs.len() + 1) != vk.gamma_abc_g1.len() {
            return Err(ark_relations::gr1cs::SynthesisError::ArityMismatch.into());
        }

        // Prepare verifying key
        let pvk = ark_groth16::prepare_verifying_key(&vk);

        // Deserialize proof
        let proof: &Proof<ark_ec::bn::Bn<ark_bn254::Config>> = &Proof::deserialize_compressed(&*proof_bytes)?;

        // Prepare public inputs with the prepared verifying key
        let prepared_inputs =
            Groth16::<Bn254>::prepare_inputs(&pvk, &unprepared_public_inputs.iter().map(|x| *x.field()).collect::<Vec<_>>())?;

        // Verify the proof with the prepared inputs
        if Groth16::<Bn254>::verify_proof_with_prepared_inputs(&pvk, proof, &prepared_inputs)? {
            Ok(())
        } else {
            Err(Groth16Error::VerificationFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GAMMA_ABC_G1_LEN_PREFIX_BYTES, GROTH16_GAMMA_ABC_G1_ELEMENT_SCRIPT_UNITS, Groth16Error, VK_FIXED_PREFIX_LEN};
    use crate::{
        EngineFlags, data_stack::Stack, hex, runtime_resource_meter::RuntimeResourceMeter, zk_precompiles::{ZkPrecompile, groth16::Groth16Precompile, tests::helpers::load_groth_fields}
    };
    use ark_bn254::{Bn254, G1Affine, G2Affine};
    use ark_groth16::VerifyingKey;
    use ark_serialize::{CanonicalDeserialize, CanonicalSerialize, Compress};
    use kaspa_consensus_core::mass::ScriptUnits;
    use kaspa_txscript_errors::TxScriptError;

    fn hardened_flags() -> EngineFlags {
        EngineFlags { covenants_enabled: true, zk_hardening_enabled: true, ..Default::default() }
    }

    fn legacy_flags() -> EngineFlags {
        EngineFlags { covenants_enabled: true, zk_hardening_enabled: false, ..Default::default() }
    }

    fn stack_with_groth_fields(vk: Vec<u8>, proof: Vec<u8>, inputs: Vec<Vec<u8>>) -> Stack {
        let mut stack = Stack::new(Vec::new(), true);
        for input in inputs.iter().rev() {
            stack.push(input.clone().into()).unwrap();
        }
        stack.push_item(inputs.len() as i32).unwrap();
        stack.push(proof.into()).unwrap();
        stack.push(vk.into()).unwrap();
        stack
    }

    #[test]
    fn check_sizes() {
        assert_eq!(G1Affine::default().serialized_size(Compress::Yes), 32);
        assert_eq!(G2Affine::default().serialized_size(Compress::Yes), 64);
    }

    #[test]
    fn check_vec_prefix() {
        let v: Vec<u8> = vec![];
        let mut buf = Vec::new();
        v.serialize_compressed(&mut buf).unwrap();
        assert_eq!(buf.len(), 8); // empty Vec serializes to just the length prefix
        assert_eq!(buf, [0u8; 8]); // length 0 as LE u64

        let v: Vec<u8> = vec![0xAA];
        let mut buf = Vec::new();
        v.serialize_compressed(&mut buf).unwrap();
        assert_eq!(&buf[..8], &[1, 0, 0, 0, 0, 0, 0, 0]); // length 1 LE u64
        assert_eq!(buf[8], 0xAA);
    }

    fn vk_with_gamma_abc_count(count: usize) -> Vec<u8> {
        let vk = VerifyingKey::<Bn254> {
            alpha_g1: G1Affine::default(),
            beta_g2: G2Affine::default(),
            gamma_g2: G2Affine::default(),
            delta_g2: G2Affine::default(),
            gamma_abc_g1: vec![G1Affine::default(); count],
        };
        let mut bytes = Vec::new();
        vk.serialize_compressed(&mut bytes).expect("serialize VK");
        bytes
    }

    #[test]
    fn verify_zk_rejects_arity_mismatch_before_meter_charge() {
        let vk_bytes = vk_with_gamma_abc_count(5);

        let mut stack = Stack::new(Vec::new(), true);
        stack.push_item(0i32).unwrap();
        stack.push(vec![0u8; 128].into()).unwrap();
        stack.push(vk_bytes.into()).unwrap();

        let mut meter = RuntimeResourceMeter::new_script_units(ScriptUnits(0), ScriptUnits(0));
        let err = Groth16Precompile::verify_zk(&mut stack, &mut meter, hardened_flags()).expect_err("arity mismatch must be rejected");
        match err {
            Groth16Error::ArkR1CS(ark_relations::gr1cs::SynthesisError::ArityMismatch) => {}
            other => panic!("expected ArityMismatch before meter charge, got: {other:?}"),
        }
        assert_eq!(meter.used_script_units(), ScriptUnits(0));
    }

    #[test]
    fn verify_zk_rejects_over_budget_vk_via_meter() {
        const PER_INPUT_BUDGET: ScriptUnits = ScriptUnits(200_000);
        const COUNT: usize = 5;
        let vk_bytes = vk_with_gamma_abc_count(COUNT);

        let mut stack = Stack::new(Vec::new(), true);
        for _ in 0..COUNT - 1 {
            stack.push(vec![0u8; 32].into()).unwrap();
        }
        stack.push_item((COUNT - 1) as i32).unwrap();
        stack.push(vec![0u8; 128].into()).unwrap();
        stack.push(vk_bytes.into()).unwrap();

        let mut meter = RuntimeResourceMeter::new_script_units(ScriptUnits(0), PER_INPUT_BUDGET);

        let expected_charge = (COUNT as u64).saturating_mul(GROTH16_GAMMA_ABC_G1_ELEMENT_SCRIPT_UNITS);
        assert!(expected_charge > PER_INPUT_BUDGET.0, "gamma_abc charge {expected_charge} must exceed budget {}", PER_INPUT_BUDGET.0);

        let err = Groth16Precompile::verify_zk(&mut stack, &mut meter, hardened_flags()).expect_err("over-budget VK must be rejected");
        match err {
            Groth16Error::FromTxScript(TxScriptError::ExceededCommittedScriptUnits { used, limit }) => {
                assert_eq!(limit, PER_INPUT_BUDGET.0);
                assert_eq!(used, expected_charge);
            }
            other => panic!("expected ExceededCommittedScriptUnits for gamma_abc_g1 element count = {COUNT}, got: {other:?}"),
        }
    }

    /// validate that abc g1 length is at the offset we expect it is
    #[test]
    fn gamma_abc_g1_length_prefix_lives_at_expected_offset() {
        for &count in &[0usize, 1, 5, 6, 42] {
            let bytes = vk_with_gamma_abc_count(count);
            assert!(bytes.len() >= VK_FIXED_PREFIX_LEN + GAMMA_ABC_G1_LEN_PREFIX_BYTES);
            let len_slice: [u8; GAMMA_ABC_G1_LEN_PREFIX_BYTES] =
                bytes[VK_FIXED_PREFIX_LEN..VK_FIXED_PREFIX_LEN + GAMMA_ABC_G1_LEN_PREFIX_BYTES].try_into().unwrap();
            assert_eq!(u64::from_le_bytes(len_slice), count as u64, "mismatch for expected gamma_abc_g1 element count = {count}");
        }
    }

    #[test]
    fn ark_vk_deserialize_reads_gamma_abc_g1_len_from_expected_offset() {
        // The precompile meters large VKs by reading the Ark-serialized
        // gamma_abc_g1 Vec length before deserializing the VK. This locks that
        // our offset is the same length prefix Ark later uses for deserialization.
        let mut two_elem_bytes = vk_with_gamma_abc_count(5);
        let two_elem_len =
            VK_FIXED_PREFIX_LEN + GAMMA_ABC_G1_LEN_PREFIX_BYTES + 2 * G1Affine::default().serialized_size(Compress::Yes);
        two_elem_bytes[VK_FIXED_PREFIX_LEN..VK_FIXED_PREFIX_LEN + GAMMA_ABC_G1_LEN_PREFIX_BYTES].copy_from_slice(&2u64.to_le_bytes());
        two_elem_bytes.truncate(two_elem_len);

        let vk =
            VerifyingKey::<Bn254>::deserialize_compressed(&*two_elem_bytes).expect("Ark should deserialize two gamma_abc_g1 elements");

        assert_eq!(vk.gamma_abc_g1.len(), 2);

        let mut five_elem_prefix_with_two_elems = two_elem_bytes;
        five_elem_prefix_with_two_elems[VK_FIXED_PREFIX_LEN..VK_FIXED_PREFIX_LEN + GAMMA_ABC_G1_LEN_PREFIX_BYTES]
            .copy_from_slice(&5u64.to_le_bytes());
        VerifyingKey::<Bn254>::deserialize_compressed(&*five_elem_prefix_with_two_elems)
            .expect_err("Ark should reject when the prefix asks for five gamma_abc_g1 elements but only two are present");
    }

    #[test]
    fn try_verify_stack() {
        let (vk, proof, inputs) = load_groth_fields();
        let mut stack = stack_with_groth_fields(vk, proof, inputs);
        let mut meter = RuntimeResourceMeter::new_script_units(ScriptUnits(0), ScriptUnits(u64::MAX));
        Groth16Precompile::verify_zk(&mut stack, &mut meter, hardened_flags()).unwrap();
    }

    #[test]
    fn legacy_verify_path_accepts_canonical_proof_without_metering() {
        let (vk, proof, inputs) = load_groth_fields();
        let mut stack = stack_with_groth_fields(vk, proof, inputs);

        let mut meter = RuntimeResourceMeter::new_script_units(ScriptUnits(0), ScriptUnits(0));
        Groth16Precompile::verify_zk(&mut stack, &mut meter, legacy_flags()).expect("legacy path must accept canonical proof");
        assert_eq!(meter.used_script_units(), ScriptUnits(0), "legacy path must not consume meter units");
    }

    #[test]
    fn legacy_verify_path_tolerates_oversized_fr_push() {
        let (vk, proof, mut inputs) = load_groth_fields();
        inputs[0].extend_from_slice(&[0xAB; 32]);
        let mut stack = stack_with_groth_fields(vk, proof, inputs);

        let mut meter = RuntimeResourceMeter::new_script_units(ScriptUnits(0), ScriptUnits(u64::MAX));
        Groth16Precompile::verify_zk(&mut stack, &mut meter, legacy_flags())
            .expect("legacy path must accept 64-byte Fr push (silent truncation)");
    }

    #[test]
    fn hardened_verify_path_rejects_oversized_fr_push() {
        let vk_bytes = vk_with_gamma_abc_count(6); // 5 pub inputs + 1
        let oversized_input = vec![0u8; 64];

        let mut stack = Stack::new(Vec::new(), true);
        for _ in 0..4 {
            stack.push(vec![0u8; 32].into()).unwrap();
        }
        stack.push(oversized_input.into()).unwrap(); // 64-byte push, must be rejected
        stack.push_item(5i32).unwrap();
        stack.push(vec![0u8; 128].into()).unwrap();
        stack.push(vk_bytes.into()).unwrap();

        let mut meter = RuntimeResourceMeter::new_script_units(ScriptUnits(0), ScriptUnits(u64::MAX));
        let err = Groth16Precompile::verify_zk(&mut stack, &mut meter, hardened_flags())
            .expect_err("hardened path must reject oversized Fr push");
        match err {
            Groth16Error::FromTxScript(TxScriptError::ZkIntegrity(msg)) if msg.contains("Invalid Fr length") => {}
            other => panic!("expected Invalid Fr length error, got: {other:?}"),
        }
    }
}
