// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) DUSK NETWORK. All rights reserved.

//! A Proof stores the commitments to all of the elements that are needed to
//! univocally identify a prove of some statement.
//!
//! This module contains the implementation of the `StandardComposer`s [`Proof`]
//! structure and it's methods.

use crate::{
    commitment::HomomorphicCommitment,
    error::Error,
    proof_system::{
        ecc::{CurveAddition, FixedBaseScalarMul},
        linearisation_poly::ProofEvaluations,
        logic::Logic,
        range::Range,
        GateConstraint, VerifierKey as PlonkVerifierKey,
    },
    util::EvaluationDomainExt,
};
use ark_ec::TEModelParameters;

use ark_ff::{fields::batch_inversion, to_bytes, FftField, PrimeField};
use ark_poly::{EvaluationDomain, GeneralEvaluationDomain};
use ark_poly_commit::QuerySet;
use ark_serialize::{
    CanonicalDeserialize, CanonicalSerialize, Read, SerializationError, Write,
};
use merlin::Transcript;

use super::pi::PublicInputs;

use ark_marlin::rng::FiatShamirRng;
use digest::Digest;

/// A [`Proof`] is a composition of `Commitment`s to the Witness, Permutation,
/// Quotient, Shifted and Opening polynomials as well as the
/// `ProofEvaluations`.
#[derive(CanonicalDeserialize, CanonicalSerialize, derivative::Derivative)]
#[derivative(
    Clone(
        bound = "PC::Commitment: Clone, PC::Proof: Clone, PC::BatchProof: Clone"
    ),
    Debug(
        bound = "PC::Commitment: core::fmt::Debug, PC::Proof: core::fmt::Debug, PC::BatchProof: core::fmt::Debug"
    ),
    Default(
        bound = "PC::Commitment: Default, PC::Proof: Default, PC::BatchProof: Default"
    ),
    Eq(bound = "PC::Commitment: Eq, PC::Proof: Eq, PC::BatchProof: Eq"),
    PartialEq(
        bound = "PC::Commitment: PartialEq, PC::Proof: PartialEq, PC::BatchProof: PartialEq"
    )
)]
pub struct Proof<F, PC>
where
    F: PrimeField,
    PC: HomomorphicCommitment<F>,
{
    /// Commitment to the witness polynomial for the left wires.
    pub(crate) a_comm: PC::Commitment,

    /// Commitment to the witness polynomial for the right wires.
    pub(crate) b_comm: PC::Commitment,

    /// Commitment to the witness polynomial for the output wires.
    pub(crate) c_comm: PC::Commitment,

    /// Commitment to the witness polynomial for the fourth wires.
    pub(crate) d_comm: PC::Commitment,

    /// Commitment to the permutation polynomial.
    pub(crate) z_comm: PC::Commitment,

    /// Commitment to the lookup query polynomial.
    pub(crate) f_comm: PC::Commitment,

    /// Commitment to first half of sorted polynomial
    pub(crate) h_1_comm: PC::Commitment,

    /// Commitment to second half of sorted polynomial
    pub(crate) h_2_comm: PC::Commitment,

    /// Commitment to the lookup permutation polynomial.
    pub(crate) z_2_comm: PC::Commitment,

    /// Commitment to the quotient polynomial.
    pub(crate) t_1_comm: PC::Commitment,

    /// Commitment to the quotient polynomial.
    pub(crate) t_2_comm: PC::Commitment,

    /// Commitment to the quotient polynomial.
    pub(crate) t_3_comm: PC::Commitment,

    /// Commitment to the quotient polynomial.
    pub(crate) t_4_comm: PC::Commitment,

    /// Subset of all of the evaluations added to the proof.
    pub(crate) evaluations: ProofEvaluations<F>,

    /// Batch opening proof of the aggregated witnesses
    pub(crate) batch_opening: PC::BatchProof,
}

impl<F, PC> Proof<F, PC>
where
    F: PrimeField,
    PC: HomomorphicCommitment<F>,
{
    /// Performs the verification of a [`Proof`] returning a boolean result.
    pub(crate) fn verify<P, D: Digest>(
        &self,
        plonk_verifier_key: &PlonkVerifierKey<F, PC>,
        _transcript: &mut Transcript,
        verifier_key: &PC::VerifierKey,
        pub_inputs: &PublicInputs<F>,
    ) -> Result<(), Error>
    where
        P: TEModelParameters<BaseField = F>,
    {
        let domain =
            GeneralEvaluationDomain::<F>::new(plonk_verifier_key.n).ok_or(Error::InvalidEvalDomainSize {
                log_size_of_group: plonk_verifier_key.n.trailing_zeros(),
                adicity: <<F as FftField>::FftParams as ark_ff::FftParameters>::TWO_ADICITY,
            })?;

        pub const PROTOCOL_NAME: &[u8] = b"Plonk";
        let mut fs_rng =
            FiatShamirRng::<D>::from_seed(&to_bytes![&PROTOCOL_NAME].unwrap());

        // Append Public Inputs to the transcript
        // Add them in evaluations form since DensePolynomial doesn't implement
        // to_bytes
        fs_rng.absorb(&to_bytes![pub_inputs.as_evals()].unwrap());

        // Subgroup checks are done when the proof is deserialised.

        // In order for the Verifier and Prover to have the same view in the
        // non-interactive setting Both parties must commit the same
        // elements into the transcript Below the verifier will simulate
        // an interaction with the prover by adding the same elements
        // that the prover added into the transcript, hence generating the
        // same challenges
        //
        // Add commitment to witness polynomials to transcript

        let w_commits = vec![
            self.a_comm.clone(),
            self.b_comm.clone(),
            self.c_comm.clone(),
            self.d_comm.clone(),
        ];
        fs_rng.absorb(&to_bytes![w_commits].unwrap());

        // Compute table compression challenge `zeta`.
        let zeta = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![zeta].unwrap());

        // Add f_poly commitment to transcript
        fs_rng.absorb(&to_bytes![self.f_comm].unwrap());

        // Add h polynomials to transcript
        fs_rng.absorb(&to_bytes![self.h_1_comm, self.h_2_comm].unwrap());

        // Compute permutation challenges and add them to transcript

        // Compute permutation challenge `beta`.
        let beta = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![beta].unwrap());

        // Compute permutation challenge `gamma`.
        let gamma = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![gamma].unwrap());

        // Compute permutation challenge `delta`.
        let delta = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![delta].unwrap());

        let epsilon = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![epsilon].unwrap());

        // Challenges must be different
        assert!(beta != gamma, "challenges must be different");
        assert!(beta != delta, "challenges must be different");
        assert!(beta != epsilon, "challenges must be different");
        assert!(gamma != delta, "challenges must be different");
        assert!(gamma != epsilon, "challenges must be different");
        assert!(delta != epsilon, "challenges must be different");

        // Add commitment to permutation polynomial to transcript
        fs_rng.absorb(&to_bytes![self.z_comm].unwrap());

        let alpha = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![alpha].unwrap());

        let range_sep_challenge = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![range_sep_challenge].unwrap());

        let logic_sep_challenge = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![logic_sep_challenge].unwrap());

        let fixed_base_sep_challenge = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![fixed_base_sep_challenge].unwrap());

        let var_base_sep_challenge = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![var_base_sep_challenge].unwrap());

        let lookup_sep_challenge = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![lookup_sep_challenge].unwrap());
        fs_rng.absorb(
            &to_bytes![
                self.t_1_comm,
                self.t_2_comm,
                self.t_3_comm,
                self.t_4_comm
            ]
            .unwrap(),
        );

        let z_challenge = F::rand(&mut fs_rng);
        fs_rng.absorb(&to_bytes![z_challenge].unwrap());

        // Compute zero polynomial evaluated at `z_challenge`
        let z_h_eval = domain.evaluate_vanishing_polynomial(z_challenge);

        // Compute first lagrange polynomial evaluated at `z_challenge`
        let l1_eval =
            compute_first_lagrange_evaluation(&domain, &z_h_eval, &z_challenge);

        let r0 = self.compute_r0(
            &domain,
            &pub_inputs.as_evals(),
            alpha,
            beta,
            gamma,
            delta,
            epsilon,
            z_challenge,
            l1_eval,
            self.evaluations.perm_evals.permutation_eval,
            self.evaluations.lookup_evals.z2_next_eval,
            self.evaluations.lookup_evals.h1_next_eval,
            self.evaluations.lookup_evals.h2_eval,
            lookup_sep_challenge,
        );

        fs_rng.absorb(
            &to_bytes![
                self.evaluations.wire_evals.a_eval,
                self.evaluations.wire_evals.b_eval,
                self.evaluations.wire_evals.c_eval,
                self.evaluations.wire_evals.d_eval,
                self.evaluations.perm_evals.left_sigma_eval,
                self.evaluations.perm_evals.right_sigma_eval,
                self.evaluations.perm_evals.out_sigma_eval,
                self.evaluations.perm_evals.permutation_eval,
                self.evaluations.lookup_evals.f_eval,
                self.evaluations.lookup_evals.q_lookup_eval,
                self.evaluations.lookup_evals.z2_next_eval,
                self.evaluations.lookup_evals.h1_eval,
                self.evaluations.lookup_evals.h1_next_eval,
                self.evaluations.lookup_evals.h2_eval
            ]
            .unwrap(),
        );

        self.evaluations
            .custom_evals
            .vals
            .iter()
            .for_each(|(_, eval)| {
                fs_rng.absorb(&to_bytes![eval].unwrap());
            });

        // Compute linearisation commitment
        let lin_comm = self.compute_linearisation_commitment::<P>(
            &domain,
            alpha,
            beta,
            gamma,
            delta,
            epsilon,
            zeta,
            range_sep_challenge,
            logic_sep_challenge,
            fixed_base_sep_challenge,
            var_base_sep_challenge,
            lookup_sep_challenge,
            z_challenge,
            l1_eval,
            plonk_verifier_key,
        );

        let zeta_sq = zeta.square();
        let table_comm = PC::multi_scalar_mul(
            &[
                plonk_verifier_key.lookup.table_1.clone(),
                plonk_verifier_key.lookup.table_2.clone(),
                plonk_verifier_key.lookup.table_3.clone(),
                plonk_verifier_key.lookup.table_4.clone(),
            ],
            &[F::one(), zeta, zeta_sq, zeta_sq * zeta],
        );

        // Commitment Scheme
        // Now we delegate computation to the commitment scheme by batch
        // checking two proofs.
        //
        // The `AggregateProof`, which proves that all the necessary
        // polynomials evaluated at `z_challenge` are
        // correct and a `Proof` which is proof that the
        // permutation polynomial evaluated at the shifted root of unity is
        // correct

        // Generation of the first aggregated proof: It ensures that the
        // polynomials evaluated at `z_challenge` are correct.

        // Reconstruct the Aggregated Proof commitments and evals
        // The proof consists of the witness commitment with no blinder

        // Compute aggregate witness to polynomials evaluated at the evaluation
        // challenge `z`

        /*************************************************************** */
        let separation_challenge = F::rand(&mut fs_rng);

        let w_commits = [
            lin_comm,
            plonk_verifier_key.permutation.left_sigma.clone(),
            plonk_verifier_key.permutation.right_sigma.clone(),
            plonk_verifier_key.permutation.out_sigma.clone(),
            self.f_comm.clone(),
            self.h_2_comm.clone(),
            table_comm.clone(),
            self.a_comm.clone(),
            self.b_comm.clone(),
            self.c_comm.clone(),
            self.d_comm.clone(),
        ];

        let w_commits = w_commits
            .iter()
            .enumerate()
            .map(|(i, c)| {
                ark_poly_commit::LabeledCommitment::new(
                    format!("w_{}", i),
                    c.clone(),
                    None,
                )
            })
            .collect::<Vec<_>>();

        let saw_commits = [
            self.z_comm.clone(),
            self.a_comm.clone(),
            self.b_comm.clone(),
            self.d_comm.clone(),
            self.h_1_comm.clone(),
            self.z_2_comm.clone(),
            table_comm,
        ];

        let saw_commits = saw_commits
            .iter()
            .enumerate()
            .map(|(i, c)| {
                ark_poly_commit::LabeledCommitment::new(
                    format!("saw_{}", i),
                    c.clone(),
                    None,
                )
            })
            .collect::<Vec<_>>();

        let mut query_set = QuerySet::new();
        let z_label = String::from("z");
        let omega_z_label = String::from("omega_z");

        for c in &w_commits {
            query_set
                .insert((c.label().clone(), (z_label.clone(), z_challenge)));
        }
        for c in &saw_commits {
            query_set.insert((
                c.label().clone(),
                (omega_z_label.clone(), z_challenge * domain.element(1)),
            ));
        }

        let w_evals = [
            -r0,
            self.evaluations.perm_evals.left_sigma_eval,
            self.evaluations.perm_evals.right_sigma_eval,
            self.evaluations.perm_evals.out_sigma_eval,
            self.evaluations.lookup_evals.f_eval,
            self.evaluations.lookup_evals.h2_eval,
            self.evaluations.lookup_evals.table_eval,
            self.evaluations.wire_evals.a_eval,
            self.evaluations.wire_evals.b_eval,
            self.evaluations.wire_evals.c_eval,
            self.evaluations.wire_evals.d_eval,
        ];

        let saw_evals = [
            self.evaluations.perm_evals.permutation_eval,
            self.evaluations.custom_evals.get("a_next_eval"),
            self.evaluations.custom_evals.get("b_next_eval"),
            self.evaluations.custom_evals.get("d_next_eval"),
            self.evaluations.lookup_evals.h1_next_eval,
            self.evaluations.lookup_evals.z2_next_eval,
            self.evaluations.lookup_evals.table_next_eval,
        ];

        let mut evaluations = ark_poly_commit::Evaluations::new();

        for (c, &eval) in w_commits.iter().zip(w_evals.iter()) {
            evaluations.insert((c.label().clone(), z_challenge), eval);
        }

        for (c, &eval) in saw_commits.iter().zip(saw_evals.iter()) {
            evaluations.insert(
                (c.label().clone(), z_challenge * domain.element(1)),
                eval,
            );
        }

        let commitments = w_commits.iter().chain(saw_commits.iter());

        match PC::batch_check(
            verifier_key,
            commitments,
            &query_set,
            &evaluations,
            &self.batch_opening,
            separation_challenge,
            &mut fs_rng,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::ProofVerificationError),
            Err(e) => panic!("{:?}", e),
        }
    }

    fn compute_r0(
        &self,
        domain: &GeneralEvaluationDomain<F>,
        pub_inputs: &[F],
        alpha: F,
        beta: F,
        gamma: F,
        delta: F,
        epsilon: F,
        z_challenge: F,
        l1_eval: F,
        z_hat_eval: F,
        z2_next_eval: F,
        h1_next_eval: F,
        h2_eval: F,
        lookup_sep_challenge: F,
    ) -> F {
        // Compute the public input polynomial evaluated at `z_challenge`
        let pi_eval = compute_barycentric_eval(pub_inputs, z_challenge, domain);

        let alpha_sq = alpha.square();

        let lookup_sep_challenge_sq = lookup_sep_challenge.square();
        let lookup_sep_challenge_cu =
            lookup_sep_challenge_sq * lookup_sep_challenge;

        // a + beta * sigma_1 + gamma
        let beta_sig1 = beta * self.evaluations.perm_evals.left_sigma_eval;
        let b_0 = self.evaluations.wire_evals.a_eval + beta_sig1 + gamma;

        // b + beta * sigma_2 + gamma
        let beta_sig2 = beta * self.evaluations.perm_evals.right_sigma_eval;
        let b_1 = self.evaluations.wire_evals.b_eval + beta_sig2 + gamma;

        // c + beta * sigma_3 + gamma
        let beta_sig3 = beta * self.evaluations.perm_evals.out_sigma_eval;
        let b_2 = self.evaluations.wire_evals.c_eval + beta_sig3 + gamma;

        // ((d + gamma) * z_hat) * alpha
        let b_3 =
            (self.evaluations.wire_evals.d_eval + gamma) * z_hat_eval * alpha;

        let b = b_0 * b_1 * b_2 * b_3;

        // l_1(z) * alpha^2
        let c = l1_eval * alpha_sq;

        let epsilon_one_plus_delta = epsilon * (F::one() + delta);

        let d_0 = lookup_sep_challenge_sq * z2_next_eval;
        let d_1 = epsilon_one_plus_delta + delta * h2_eval;
        let d_2 = epsilon_one_plus_delta + h2_eval + delta * h1_next_eval;
        let d = d_0 * d_1 * d_2;

        let e = lookup_sep_challenge_cu * l1_eval;

        // Return r_0
        pi_eval - b - c - d - e
    }

    /// Computes the commitment to `[r]_1`.
    fn compute_linearisation_commitment<P>(
        &self,
        domain: &GeneralEvaluationDomain<F>,
        alpha: F,
        beta: F,
        gamma: F,
        delta: F,
        epsilon: F,
        zeta: F,
        range_sep_challenge: F,
        logic_sep_challenge: F,
        fixed_base_sep_challenge: F,
        var_base_sep_challenge: F,
        lookup_sep_challenge: F,
        z_challenge: F,
        l1_eval: F,
        plonk_verifier_key: &PlonkVerifierKey<F, PC>,
    ) -> PC::Commitment
    where
        P: TEModelParameters<BaseField = F>,
    {
        //    6 for arithmetic
        // +  1 for range
        // +  1 for logic
        // +  1 for fixed base mul
        // +  1 for curve add
        // +  3 for lookups
        // +  2 for permutation
        // +  4 for each piece of the quotient poly
        // = 19 total scalars and points

        let mut scalars = Vec::with_capacity(19);
        let mut points = Vec::with_capacity(19);

        plonk_verifier_key
            .arithmetic
            .compute_linearisation_commitment(
                &mut scalars,
                &mut points,
                &self.evaluations,
            );
        Range::extend_linearisation_commitment::<PC>(
            &plonk_verifier_key.range_selector_commitment,
            range_sep_challenge,
            &self.evaluations,
            &mut scalars,
            &mut points,
        );

        Logic::extend_linearisation_commitment::<PC>(
            &plonk_verifier_key.logic_selector_commitment,
            logic_sep_challenge,
            &self.evaluations,
            &mut scalars,
            &mut points,
        );

        FixedBaseScalarMul::<_, P>::extend_linearisation_commitment::<PC>(
            &plonk_verifier_key.fixed_group_add_selector_commitment,
            fixed_base_sep_challenge,
            &self.evaluations,
            &mut scalars,
            &mut points,
        );
        CurveAddition::<_, P>::extend_linearisation_commitment::<PC>(
            &plonk_verifier_key.variable_group_add_selector_commitment,
            var_base_sep_challenge,
            &self.evaluations,
            &mut scalars,
            &mut points,
        );
        plonk_verifier_key.lookup.compute_linearisation_commitment(
            &mut scalars,
            &mut points,
            &self.evaluations,
            (delta, epsilon, zeta),
            lookup_sep_challenge,
            l1_eval,
            self.z_2_comm.clone(),
            self.h_1_comm.clone(),
        );
        plonk_verifier_key
            .permutation
            .compute_linearisation_commitment(
                &mut scalars,
                &mut points,
                &self.evaluations,
                z_challenge,
                (alpha, beta, gamma),
                l1_eval,
                self.z_comm.clone(),
            );

        // Second part
        let vanishing_poly_eval =
            domain.evaluate_vanishing_polynomial(z_challenge);
        // z_challenge ^ n
        let z_challenge_to_n = vanishing_poly_eval + F::one();

        let t_1_scalar = -vanishing_poly_eval;
        let t_2_scalar = t_1_scalar * z_challenge_to_n;
        let t_3_scalar = t_2_scalar * z_challenge_to_n;
        let t_4_scalar = t_3_scalar * z_challenge_to_n;
        scalars.extend_from_slice(&[
            t_1_scalar, t_2_scalar, t_3_scalar, t_4_scalar,
        ]);
        points.extend_from_slice(&[
            self.t_1_comm.clone(),
            self.t_2_comm.clone(),
            self.t_3_comm.clone(),
            self.t_4_comm.clone(),
        ]);

        PC::multi_scalar_mul(&points, &scalars)
    }
}

/// The first lagrange polynomial has the expression:
///
/// ```text
/// L_0(X) = mul_from_1_to_(n-1) [(X - omega^i) / (1 - omega^i)]
/// ```
///
/// with `omega` being the generator of the domain (the `n`th root of unity).
///
/// We use two equalities:
///   1. `mul_from_2_to_(n-1) [1 / (1 - omega^i)] = 1 / n`
///   2. `mul_from_2_to_(n-1) [(X - omega^i)] = (X^n - 1) / (X - 1)`
/// to obtain the expression:
///
/// ```text
/// L_0(X) = (X^n - 1) / n * (X - 1)
/// ```
pub fn compute_first_lagrange_evaluation<F>(
    domain: &GeneralEvaluationDomain<F>,
    z_h_eval: &F,
    z_challenge: &F,
) -> F
where
    F: PrimeField,
{
    let n_fr = F::from(domain.size() as u64);
    let denom = n_fr * (*z_challenge - F::one());
    *z_h_eval * denom.inverse().unwrap()
}

fn compute_barycentric_eval<F>(
    evaluations: &[F],
    point: F,
    domain: &GeneralEvaluationDomain<F>,
) -> F
where
    F: PrimeField,
{
    let numerator =
        domain.evaluate_vanishing_polynomial(point) * domain.size_inv();
    let range = 0..evaluations.len();

    let non_zero_evaluations = range
        .filter(|&i| {
            let evaluation = &evaluations[i];
            evaluation != &F::zero()
        })
        .collect::<Vec<_>>();

    // Only compute the denominators with non-zero evaluations
    let range = 0..non_zero_evaluations.len();

    let group_gen_inv = domain.group_gen_inv();
    let mut denominators = range
        .clone()
        .map(|i| {
            // index of non-zero evaluation
            let index = non_zero_evaluations[i];
            (group_gen_inv.pow(&[index as u64, 0, 0, 0]) * point) - F::one()
        })
        .collect::<Vec<_>>();
    batch_inversion(&mut denominators);

    let result: F = range
        .map(|i| {
            let eval_index = non_zero_evaluations[i];
            let eval = evaluations[eval_index];
            denominators[i] * eval
        })
        .sum();

    result * numerator
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::batch_test_kzg;
    use ark_bls12_377::Bls12_377;
    use ark_bls12_381::Bls12_381;

    fn test_serde_proof<F, P, PC>()
    where
        F: PrimeField,
        P: TEModelParameters<BaseField = F>,
        PC: HomomorphicCommitment<F>,
        Proof<F, PC>: std::fmt::Debug + PartialEq,
    {
        let proof =
            crate::constraint_system::helper::gadget_tester::<F, P, PC>(
                |_: &mut crate::constraint_system::StandardComposer<F, P>| {},
                200,
            )
            .expect("Empty circuit failed");

        let mut proof_bytes = vec![];
        proof.serialize(&mut proof_bytes).unwrap();

        let obtained_proof =
            Proof::<F, PC>::deserialize(proof_bytes.as_slice()).unwrap();

        assert_eq!(proof, obtained_proof);
    }

    // Bls12-381 tests
    batch_test_kzg!(
        [test_serde_proof],
        [] => (
            Bls12_381, ark_ed_on_bls12_381::EdwardsParameters
        )
    );
    // Bls12-377 tests
    batch_test_kzg!(
        [test_serde_proof],
        [] => (
            Bls12_377, ark_ed_on_bls12_377::EdwardsParameters
        )
    );
}
