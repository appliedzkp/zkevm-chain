use crate::ProverParams;
use halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    halo2curves::bn256::{Bn256, Fq, Fr, G1Affine},
    plonk::{self, Circuit, ConstraintSystem},
    poly::commitment::ParamsProver,
};
use itertools::Itertools;
use rand::Rng;
use snark_verifier::loader::halo2::halo2_wrong_ecc::{
    self,
    integer::rns::Rns,
    maingate::{
        MainGate, MainGateConfig, MainGateInstructions, RangeChip, RangeConfig, RangeInstructions,
        RegionCtx,
    },
    EccConfig,
};
use snark_verifier::pcs::kzg::KzgAccumulator;
use snark_verifier::pcs::kzg::LimbsEncodingInstructions;
use snark_verifier::{
    loader::{self, native::NativeLoader},
    pcs::{kzg, AccumulationScheme, AccumulationSchemeProver},
    system,
    util::arithmetic::{fe_to_limbs, FieldExt},
    verifier::plonk::PlonkProtocol,
    verifier::{self, SnarkVerifier},
};
use std::rc::Rc;

const LIMBS: usize = 4;
const BITS: usize = 68;

type As = kzg::KzgAs<Bn256, kzg::Gwc19>;
pub type PlonkSuccinctVerifier =
    verifier::plonk::PlonkSuccinctVerifier<As, kzg::LimbsEncoding<LIMBS, BITS>>;
pub type PlonkVerifier = verifier::plonk::PlonkVerifier<As, kzg::LimbsEncoding<LIMBS, BITS>>;

const T: usize = 5;
const RATE: usize = 4;
const R_F: usize = 8;
const R_P: usize = 60;

type Svk = kzg::KzgSuccinctVerifyingKey<G1Affine>;
type BaseFieldEccChip = halo2_wrong_ecc::BaseFieldEccChip<G1Affine, LIMBS, BITS>;
type Halo2Loader<'a> = loader::halo2::Halo2Loader<'a, G1Affine, BaseFieldEccChip>;
pub type PoseidonTranscript<L, S> =
    system::halo2::transcript::halo2::PoseidonTranscript<G1Affine, L, S, T, RATE, R_F, R_P>;

pub struct Snark {
    pub protocol: PlonkProtocol<G1Affine>,
    pub instances: Vec<Vec<Fr>>,
    pub proof: Vec<u8>,
}

impl Snark {
    pub fn new(protocol: PlonkProtocol<G1Affine>, instances: Vec<Vec<Fr>>, proof: Vec<u8>) -> Self {
        Self {
            protocol,
            instances,
            proof,
        }
    }
}

impl From<Snark> for SnarkWitness {
    fn from(snark: Snark) -> Self {
        Self {
            protocol: snark.protocol,
            instances: snark
                .instances
                .into_iter()
                .map(|instances| instances.into_iter().map(Value::known).collect_vec())
                .collect(),
            proof: Value::known(snark.proof),
        }
    }
}

#[derive(Clone)]
pub struct SnarkWitness {
    protocol: PlonkProtocol<G1Affine>,
    instances: Vec<Vec<Value<Fr>>>,
    proof: Value<Vec<u8>>,
}

impl SnarkWitness {
    fn without_witnesses(&self) -> Self {
        SnarkWitness {
            protocol: self.protocol.clone(),
            instances: self
                .instances
                .iter()
                .map(|instances| vec![Value::unknown(); instances.len()])
                .collect(),
            proof: Value::unknown(),
        }
    }

    fn proof(&self) -> Value<&[u8]> {
        self.proof.as_ref().map(Vec::as_slice)
    }
}

pub fn aggregate<'a>(
    svk: &Svk,
    loader: &Rc<Halo2Loader<'a>>,
    snarks: &[SnarkWitness],
    as_proof: Value<&'_ [u8]>,
) -> KzgAccumulator<G1Affine, Rc<Halo2Loader<'a>>> {
    let assign_instances = |instances: &[Vec<Value<Fr>>]| {
        instances
            .iter()
            .map(|instances| {
                instances
                    .iter()
                    .map(|instance| loader.assign_scalar(*instance))
                    .collect_vec()
            })
            .collect_vec()
    };

    let accumulators = snarks
        .iter()
        .flat_map(|snark| {
            let protocol = snark.protocol.loaded(loader);
            let instances = assign_instances(&snark.instances);
            let mut transcript =
                PoseidonTranscript::<Rc<Halo2Loader>, _>::new(loader, snark.proof());
            let proof =
                PlonkSuccinctVerifier::read_proof(svk, &protocol, &instances, &mut transcript)
                    .unwrap();
            PlonkSuccinctVerifier::verify(svk, &protocol, &instances, &proof).unwrap()
        })
        .collect_vec();

    let acccumulator = {
        let mut transcript = PoseidonTranscript::<Rc<Halo2Loader>, _>::new(loader, as_proof);
        let proof = As::read_proof(&Default::default(), &accumulators, &mut transcript).unwrap();
        As::verify(&Default::default(), &accumulators, &proof).unwrap()
    };

    acccumulator
}

#[derive(Clone)]
pub struct AggregationConfig {
    main_gate_config: MainGateConfig,
    range_config: RangeConfig,
}

impl AggregationConfig {
    pub fn configure<F: FieldExt>(
        meta: &mut ConstraintSystem<F>,
        composition_bits: Vec<usize>,
        overflow_bits: Vec<usize>,
    ) -> Self {
        let main_gate_config = MainGate::<F>::configure(meta);
        let range_config =
            RangeChip::<F>::configure(meta, &main_gate_config, composition_bits, overflow_bits);
        AggregationConfig {
            main_gate_config,
            range_config,
        }
    }

    pub fn main_gate(&self) -> MainGate<Fr> {
        MainGate::new(self.main_gate_config.clone())
    }

    pub fn range_chip(&self) -> RangeChip<Fr> {
        RangeChip::new(self.range_config.clone())
    }

    pub fn ecc_chip(&self) -> BaseFieldEccChip {
        BaseFieldEccChip::new(EccConfig::new(
            self.range_config.clone(),
            self.main_gate_config.clone(),
        ))
    }
}

#[derive(Clone)]
pub struct AggregationCircuit {
    svk: Svk,
    snarks: Vec<SnarkWitness>,
    instances: Vec<Fr>,
    as_proof: Value<Vec<u8>>,
}

impl AggregationCircuit {
    pub fn new<RNG: Rng>(
        params: &ProverParams,
        snarks: impl IntoIterator<Item = Snark>,
        mut rng: RNG,
    ) -> Self {
        let svk = params.get_g()[0].into();
        let snarks = snarks.into_iter().collect_vec();

        let accumulators = snarks
            .iter()
            .flat_map(|snark| {
                let mut transcript =
                    PoseidonTranscript::<NativeLoader, _>::new(snark.proof.as_slice());
                let proof = PlonkSuccinctVerifier::read_proof(
                    &svk,
                    &snark.protocol,
                    &snark.instances,
                    &mut transcript,
                )
                .unwrap();
                PlonkSuccinctVerifier::verify(&svk, &snark.protocol, &snark.instances, &proof)
                    .unwrap()
            })
            .collect_vec();

        let (accumulator, as_proof) = {
            let mut transcript = PoseidonTranscript::<NativeLoader, _>::new(Vec::new());
            let accumulator = As::create_proof(
                &Default::default(),
                &accumulators,
                &mut transcript,
                &mut rng,
            )
            .unwrap();
            (accumulator, transcript.finalize())
        };

        let KzgAccumulator { lhs, rhs } = accumulator;
        let instances = [lhs.x, lhs.y, rhs.x, rhs.y]
            .map(fe_to_limbs::<_, _, LIMBS, BITS>)
            .concat();

        Self {
            svk,
            snarks: snarks.into_iter().map_into().collect(),
            instances,
            as_proof: Value::known(as_proof),
        }
    }

    pub fn accumulator_indices() -> Vec<(usize, usize)> {
        (0..4 * LIMBS).map(|idx| (0, idx)).collect()
    }

    pub fn num_instance() -> Vec<usize> {
        vec![4 * LIMBS]
    }

    pub fn instance(&self) -> Vec<Vec<Fr>> {
        vec![self.instances.clone()]
    }

    pub fn as_proof(&self) -> Value<&[u8]> {
        self.as_proof.as_ref().map(Vec::as_slice)
    }
}

impl Circuit<Fr> for AggregationCircuit {
    type Config = AggregationConfig;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self {
            svk: self.svk,
            snarks: self
                .snarks
                .iter()
                .map(SnarkWitness::without_witnesses)
                .collect(),
            instances: Vec::new(),
            as_proof: Value::unknown(),
        }
    }

    fn configure(meta: &mut plonk::ConstraintSystem<Fr>) -> Self::Config {
        AggregationConfig::configure(
            meta,
            vec![BITS / LIMBS],
            Rns::<Fq, Fr, LIMBS, BITS>::construct().overflow_lengths(),
        )
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fr>,
    ) -> Result<(), plonk::Error> {
        let main_gate = config.main_gate();
        let range_chip = config.range_chip();

        range_chip.load_table(&mut layouter)?;

        let accumulator_limbs = layouter.assign_region(
            || "",
            |region| {
                let ctx = RegionCtx::new(region, 0);

                let ecc_chip = config.ecc_chip();
                let loader = Halo2Loader::new(ecc_chip, ctx);
                let accumulator = aggregate(&self.svk, &loader, &self.snarks, self.as_proof());

                let accumulator_limbs = [accumulator.lhs, accumulator.rhs]
                    .iter()
                    .map(|ec_point| {
                        loader
                            .ecc_chip()
                            .assign_ec_point_to_limbs(&mut loader.ctx_mut(), ec_point.assigned())
                    })
                    .collect::<Result<Vec<_>, plonk::Error>>()?
                    .into_iter()
                    .flatten();

                Ok(accumulator_limbs)
            },
        )?;

        for (row, limb) in accumulator_limbs.enumerate() {
            main_gate.expose_public(layouter.namespace(|| ""), limb, row)?;
        }

        Ok(())
    }
}
