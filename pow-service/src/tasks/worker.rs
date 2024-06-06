use anyhow::Result;
use futures::{SinkExt, StreamExt};
use log::{debug, error, info};
use post::pow::{randomx, Prover};
use rayon::ThreadPool;
use std::{
    collections::HashMap, net::SocketAddr, ops::Range, str::FromStr, sync::Arc, time::Duration,
};
use tokio::{
    io::split,
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::sleep,
};
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::messages::codec::{PoWMessageCodec, PowMessage, PowRequest, PowResponse, Register};

const NONCES_PER_AES: u32 = 16;

pub fn calculate_pow(pool: Arc<ThreadPool>, req: PowRequest) -> HashMap<u32, u64> {
    let PowRequest {
        id: _,
        challenge,
        start_nonce,
        end_nonce,
        difficulty: _,
        pow_difficulty,
        node_id,
        pow_flags,
    } = req;
    let pow_flags = post::pow::randomx::RandomXFlag::from_str(&pow_flags).unwrap();
    debug!("generating proof with PoW flags: {pow_flags:?}");
    let pow_prover = randomx::PoW::new(pow_flags).unwrap();
    let pows: HashMap<u32, u64> = pool.install(|| {
        let x: Vec<(u32, u64)> = nonce_group_range(start_nonce..end_nonce, NONCES_PER_AES)
            .map(|nonce_group| {
                let pow = pow_prover
                    .prove(
                        nonce_group.try_into().unwrap(),
                        challenge[..8].try_into().unwrap(),
                        &pow_difficulty,
                        &node_id,
                    )
                    .unwrap();
                (nonce_group, pow)
            })
            .collect();
        let mut map = HashMap::new();
        for (k, v) in x {
            map.insert(k, v);
        }
        map
    });
    pows
}

#[inline(always)]
fn nonce_group_range(nonces: Range<u32>, per_aes: u32) -> Range<u32> {
    let start_group = nonces.start / per_aes;
    let end_group = std::cmp::max(start_group + 1, (nonces.end + per_aes - 1) / per_aes);
    start_group..end_group
}

pub async fn start_worker(addr: SocketAddr) -> Result<()> {
    let thread_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_cpus::get())
        .build()
        .unwrap();
    let worker_pool = Arc::new(thread_pool);
    let (router, handler) = oneshot::channel();
    tokio::spawn(async move {
        let _ = router.send(());
        loop {
            match TcpStream::connect(&addr).await {
                Ok(stream) => {
                    if let Err(e) = handle_connection(worker_pool.clone(), stream).await {
                        error!("error: {:?}", e);
                    }
                    error!("handle_connection exited");
                    sleep(Duration::from_secs(10)).await;
                }
                Err(e) => {
                    error!("error wen connect {}", e);
                    sleep(Duration::from_secs(30)).await;
                }
            }
        }
    });
    let _ = handler.await;
    std::future::pending::<()>().await;
    Ok(())
}

pub async fn handle_connection(worker_pool: Arc<ThreadPool>, stream: TcpStream) -> Result<()> {
    info!("connected to proxy");
    let (r, w) = split(stream);
    let mut socket_w_handler = FramedWrite::new(w, PoWMessageCodec::default());
    let mut socket_r_handler = FramedRead::new(r, PoWMessageCodec::default());
    let (tx, mut rx) = mpsc::channel::<PowMessage>(1024);
    let worker_name = format!("{:?}", gethostname::gethostname());

    let (router, handler) = oneshot::channel();
    let send_handler = tokio::spawn(async move {
        let _ = router.send(());
        while let Some(message) = rx.recv().await {
            match message {
                PowMessage::Register(register) => {
                    if let Err(e) = socket_w_handler.send(PowMessage::Register(register)).await {
                        error!("failed to send register message, {:?}", e);
                        return;
                    }
                }
                PowMessage::Response(response) => {
                    if let Err(e) = socket_w_handler.send(PowMessage::Response(response)).await {
                        error!("failed to send response message, {:?}", e);
                        return;
                    }
                }
                _ => error!("invalid message to send"),
            }
        }
    });
    let _ = handler.await;

    let task_tx = tx.clone();
    let (router, handler) = oneshot::channel();
    let receive_task_handler = tokio::spawn(async move {
        let _ = router.send(());
        while let Some(Ok(message)) = socket_r_handler.next().await {
            match message {
                PowMessage::Request(request) => {
                    let id = request.id.clone();
                    info!("new task from proxy: {}", id.clone());
                    let ciphers_pows = calculate_pow(worker_pool.clone(), request);
                    let response = PowResponse { id, ciphers_pows };
                    if let Err(e) = task_tx.send(PowMessage::Response(response)).await {
                        error!("error to send response to write channel, {}", e);
                    }
                }
                _ => {
                    error!("invalid message");
                    break;
                }
            }
        }
    });
    let _ = handler.await;

    // heart beat loop
    let heart_beat_tx = tx.clone();
    let (router, handler) = oneshot::channel();
    let heart_beat_handler = tokio::spawn(async move {
        let _ = router.send(());
        loop {
            let _ = heart_beat_tx
                .send(PowMessage::Register(Register {
                    name: worker_name.clone(),
                }))
                .await
                .unwrap();
            sleep(Duration::from_secs(30)).await;
        }
    });
    let _ = handler.await;

    let _ = tokio::join!(send_handler, receive_task_handler, heart_beat_handler);
    Ok(())
}
