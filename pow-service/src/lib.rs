use std::{
    borrow::Borrow,
    collections::HashMap,
    net::SocketAddr,
    ops::Range,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::sleep,
    time::{Duration, Instant},
};

pub use anyhow::anyhow;
use eyre::Context;
use log::{debug, error, warn};
use messages::{TaskResponse, TaskSubmit, TaskSubmitResponse};
use post::{
    config::{self, ProofConfig},
    metadata,
    pow::randomx::RandomXFlag,
    prove::{create_thread_pool, ProgressReporter, Proof, Prover, Prover8_56, ProvingParams},
    reader::read_data,
};
use rayon::iter::{ParallelBridge, ParallelIterator};

pub mod messages;
pub mod tasks;

const BLOCK_SIZE: usize = 16;

pub fn post_server_check(server: String) -> anyhow::Result<()> {
    let result = ureq::get(&format!("http://{}/health", &server)).call();
    match result {
        Ok(_) => Ok(()),
        Err(_) => Err(anyhow!("post server doesn't work")),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn generate_pow_remotely(
    node_id: [u8; 32],
    priority: u16,
    pow_address: String,
    challenge: &[u8; 32],
    params: ProvingParams,
    nonces: Range<u32>,
    pow_flags: RandomXFlag,
) -> eyre::Result<HashMap<u32, u64>> {
    let base_url = &format!("http://{}", pow_address);
    let pow_result = {
        let req = TaskSubmit {
            priority,
            challenge: *challenge,
            start_nonce: nonces.start,
            end_nonce: nonces.end,
            difficulty: params.difficulty,
            pow_difficulty: params.pow_difficulty,
            node_id,
            pow_flags: pow_flags.to_string(),
        };
        let submit_path = format!("{}/submit", base_url);
        let resp: TaskSubmitResponse = ureq::post(&submit_path)
            .send_json(req.clone())
            .unwrap()
            .into_json()
            .unwrap();
        debug!("k2pow task submitted, resp: {:?}", resp);
        let mut task_id = resp.id.clone();
        loop {
            let query_path = format!("{}/task", base_url);
            match ureq::post(&query_path)
                .send_json(TaskSubmitResponse {
                    id: task_id.clone(),
                })
                .unwrap()
                .into_json::<TaskResponse>()
            {
                Ok(response) => match response.status {
                    1 => debug!("task is in pending queue"),
                    2 => {
                        debug!("task completed");
                        break response.data.unwrap();
                    }
                    3 => {
                        warn!("task missed on server, try another submit");
                        let resp: TaskSubmitResponse = ureq::post(&submit_path)
                            .send_json(req.clone())
                            .unwrap()
                            .into_json()
                            .unwrap();
                        task_id = resp.id.clone();
                    }
                    _ => error!("bad task response status, should never happen"),
                },
                Err(e) => error!("{}", e),
            }
            sleep(Duration::from_secs(30));
            debug!("try to get k2pow 5 second later");
        }
    };
    Ok(pow_result)
}

#[allow(clippy::too_many_arguments)]
pub fn generate_proof_remotely<Reporter, Stopper>(
    datadir: &Path,
    challenge: &[u8; 32],
    cfg: ProofConfig,
    nonces_size: usize,
    cores: config::Cores,
    pow_flags: RandomXFlag,
    stop: Stopper,
    priority: u16,
    pow_address: SocketAddr,
    locker: Arc<Mutex<u8>>,
    reporter: Reporter,
) -> eyre::Result<Proof<'static>>
where
    Stopper: Borrow<AtomicBool>,
    Reporter: ProgressReporter + Send + Sync,
{
    let stop = stop.borrow();
    let metadata = metadata::load(datadir).wrap_err("loading metadata")?;
    let miner_id = &metadata.node_id;
    let params = ProvingParams::new(&metadata, &cfg)?;
    log::info!(
        "generating proof with PoW flags: {pow_flags:?}, difficulty (scaled with SU): {}, K2PoW difficulty (scaled with SU): {}",
        params.difficulty,
        hex::encode_upper(params.pow_difficulty)
    );

    let mut nonces = 0..nonces_size as u32;

    let pool = create_thread_pool(cores, |id| {
        log::error!("failed to set core affinity for thread to {id}");
        std::process::exit(1);
    })
    .wrap_err("building thread pool")?;

    let total_time = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            eyre::bail!("proof generation was stopped");
        }
        reporter.new_nonce_group(nonces.clone());

        let indexes = Mutex::new(HashMap::<u32, Vec<u64>>::new());

        let pow_time = Instant::now();
        let mut lock_guard = None;
        if priority == 1 {
            lock_guard = Some(locker.lock());
        }
        let pow_ciphers = generate_pow_remotely(
            *miner_id,
            priority,
            pow_address.clone().to_string(),
            challenge,
            params.clone(),
            nonces.clone(),
            pow_flags.clone(),
        )
        .unwrap();

        if priority != 1 {
            lock_guard = Some(locker.lock());
        }
        let prover = Prover8_56::from_pow_ciphers(
            nonces.clone(),
            challenge,
            pow_ciphers,
            params.difficulty,
        )?;
        let pow_secs = pow_time.elapsed().as_secs();
        let pow_mins = pow_secs / 60;
        log::info!("finished k2pow in {pow_mins}m {}s", pow_secs % 60);

        let read_time = Instant::now();
        let data_reader = read_data(datadir, 1024 * 1024, metadata.max_file_size)?;
        log::info!("started reading POST data, {:?}", datadir);
        let result = pool.install(|| {
            data_reader
                .par_bridge()
                .take_any_while(|_| !stop.load(Ordering::Relaxed))
                .find_map_any(|batch| {
                    let res = prover.prove(
                        &batch.data,
                        batch.pos / BLOCK_SIZE as u64,
                        |nonce, index| {
                            let mut indexes = indexes.lock().unwrap();
                            let vec = indexes.entry(nonce).or_default();
                            vec.push(index);
                            if vec.len() >= cfg.k2 as usize {
                                return Some(std::mem::take(vec));
                            }
                            None
                        },
                    );
                    reporter.finished_chunk(batch.pos, batch.data.len());

                    res
                })
        });

        if let Some(guard) = lock_guard {
            drop(guard);
        }

        let read_secs = read_time.elapsed().as_secs();
        let read_mins = read_secs / 60;
        log::info!(
            "finished reading POST data in {read_mins}m {}s",
            read_secs % 60
        );

        if let Some((nonce, indices)) = result {
            let num_labels = metadata.num_units as u64 * metadata.labels_per_unit;
            let pow = prover.get_pow(nonce).unwrap();

            let total_secs = total_time.elapsed().as_secs();
            let total_mins = total_secs / 60;

            log::info!("found proof for nonce: {nonce}, pow: {pow} with {indices:?} indices. It took {total_mins}m {}s", total_secs % 60);
            return Ok(Proof::new(nonce, &indices, num_labels, pow));
        }

        nonces = nonces.end..(nonces.end + nonces_size as u32);
    }
}
