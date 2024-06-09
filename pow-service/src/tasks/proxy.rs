use std::{
    cmp::Reverse,
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use axum::{
    error_handling::HandleErrorLayer,
    extract::{self, State},
    http::StatusCode,
    routing::{get, post},
    BoxError, Json, Router,
};
use futures::{SinkExt, StreamExt};
use log::{debug, info, warn};
use priority_queue::PriorityQueue;
use tokio::{
    io::split,
    net::{TcpListener, TcpStream},
    sync::{
        mpsc::{self, Sender},
        oneshot, RwLock,
    },
    time::{sleep, timeout},
};
use tokio_util::codec::{FramedRead, FramedWrite};
use tower::{timeout::TimeoutLayer, ServiceBuilder};
use tower_http::cors::{Any, CorsLayer};

use crate::messages::{
    codec::{PoWMessageCodec, PowMessage, PowRequest},
    ServerMessage, ServerSubmitRequest, TaskResponse, TaskSubmit, TaskSubmitResponse,
};

use super::cache::RedisClient;

#[derive(Debug, Clone)]
pub struct ServerWorker {
    pub router: Sender<ServerMessage>,
    // 1: Idle; 2: Busy
    pub status: u8,
}

impl ServerWorker {
    pub fn new(router: Sender<ServerMessage>) -> Self {
        Self { router, status: 1 }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyServer {
    pub workers: Arc<RwLock<HashMap<String, ServerWorker>>>,
    pub queue: Arc<RwLock<PriorityQueue<TaskSubmit, Reverse<u16>>>>,
    pub pending: Arc<RwLock<HashMap<String, Instant>>>,
    pub db: Arc<RwLock<RedisClient>>,
}

impl ProxyServer {
    pub fn new(redis: &str) -> Arc<Self> {
        Arc::new(Self {
            workers: Arc::new(RwLock::new(HashMap::new())),
            queue: Arc::new(RwLock::new(PriorityQueue::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            db: Arc::new(RwLock::new(RedisClient::connect(redis).unwrap())),
        })
    }

    pub async fn handle_stream(
        stream: TcpStream,
        server: Arc<Self>,
        network: String,
    ) -> Result<()> {
        let expiration = match network.as_str() {
            "mainnet" => 60 * 60,
            "testnet" => 10 * 60,
            _ => 60 * 60,
        };
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(1024);
        let mut worker_name = stream.peer_addr().unwrap().clone().to_string();
        let (r, w) = split(stream);
        let mut outbound_w = FramedWrite::new(w, PoWMessageCodec::default());
        let mut outbound_r = FramedRead::new(r, PoWMessageCodec::default());
        let mut timer = tokio::time::interval(Duration::from_secs(300));
        let _ = timer.tick().await;

        let (router, handler) = oneshot::channel();
        let worker_clone = server.clone();
        tokio::spawn(async move {
            let _ = router.send(());
            while let Some(request) = rx.recv().await {
                match request {
                    ServerMessage::ServerSubmitRequest(request) => {
                        match request.worker_name {
                            Some(name) => {
                                let _ = worker_clone
                                    .workers
                                    .write()
                                    .await
                                    .get_mut(&name)
                                    .unwrap()
                                    .status = 2;
                            }
                            None => {}
                        };

                        let TaskSubmit {
                            priority: _,
                            challenge,
                            start_nonce,
                            end_nonce,
                            difficulty,
                            pow_difficulty,
                            node_id,
                            pow_flags,
                        } = request.request;

                        let id = hex::encode(&node_id);
                        let pow_request = PowRequest {
                            id: id.clone(),
                            challenge,
                            start_nonce,
                            end_nonce,
                            difficulty,
                            pow_difficulty,
                            node_id,
                            pow_flags,
                        };
                        let _ = worker_clone
                            .pending
                            .write()
                            .await
                            .insert(id, Instant::now());
                        let send_future = outbound_w.send(PowMessage::Request(pow_request));
                        if let Err(error) = timeout(Duration::from_millis(200), send_future).await {
                            debug!("send message to worker timeout: {}", error);
                        }
                    }
                }
            }
        });
        let _ = handler.await;

        let (router, handler) = oneshot::channel();
        let worker_server = server.clone();
        tokio::spawn(async move {
            let _ = router.send(());
            loop {
                tokio::select! {
                    _ = timer.tick() => {
                        debug!("no message from worker {} for 5 mins, exit", worker_name);
                        let _ = worker_server.workers.write().await.remove(&worker_name).unwrap();
                        break;
                    },
                    result = outbound_r.next() => {
                        debug!("new message from outboud_reader {:?} of worker {}", result, worker_name);
                        match result {
                            Some(Ok(message)) => {
                                timer.reset();
                                match message {
                                    PowMessage::Register(register) => {
                                        debug!("heart beat info {:?}", register);
                                        match worker_name == register.name {
                                            true => {},
                                            false => {
                                                let worker = ServerWorker::new(tx.clone());
                                                worker_name = register.name;
                                                info!("new worker: {}", worker_name.clone());
                                                match worker_server.queue.write().await.pop() {
                                                    Some((task, _)) => {
                                                        let _ = tx.clone().send(ServerMessage::ServerSubmitRequest(ServerSubmitRequest {
                                                            worker_name: Some(worker_name.clone()),
                                                            request: task,
                                                        })).await.unwrap();
                                                    },
                                                    None => {},
                                                }
                                                let _ = worker_server.workers.write().await.insert(worker_name.clone(), worker);
                                            }
                                        }
                                    },
                                    PowMessage::Request(_) => log::error!("invalid message from worker"),
                                    PowMessage::Response(response) => {
                                        let task_id = response.id.clone();
                                        info!("task {} response from worker {}", task_id, worker_name);
                                        let task_res = TaskResponse::completed(task_id.clone(), response.ciphers_pows);
                                        let _ = worker_server.db.write().await.set_str(&task_id, &serde_json::to_string(&task_res).unwrap(), expiration);
                                        let _ = worker_server.pending.write().await.remove(&task_id);
                                        match worker_server.queue.write().await.pop() {
                                            Some((task, _)) => {
                                                let _ = tx.clone().send(ServerMessage::ServerSubmitRequest(ServerSubmitRequest {
                                                    worker_name: None,
                                                    request: task,
                                                })).await.unwrap();
                                            },
                                            None => worker_server.workers.write().await.get_mut(&worker_name).unwrap().status = 1,
                                        }
                                    },
                                }
                            },
                            _ => {
                                warn!("unknown message");
                                let _ = worker_server.workers.write().await.remove(&worker_name).unwrap();
                                break;
                            },
                        }
                    }
                }
            }
            debug!("worker {} main loop exit", worker_name);
        });
        let _ = handler.await;

        let (router, handler) = oneshot::channel();
        let rescheduling_server = server.clone();
        tokio::spawn(async move {
            let _ = router.send(());
            loop {
                let timeout_tasks: Vec<String> = rescheduling_server
                    .pending
                    .read()
                    .await
                    .iter()
                    .filter(|(_, v)| v.elapsed().as_secs() >= 600)
                    .map(|x| x.0.clone())
                    .collect();
                for task in timeout_tasks.iter() {
                    let _ = rescheduling_server.pending.write().await.remove(task);
                }
                sleep(Duration::from_secs(30)).await;
            }
        });
        let _ = handler.await;

        let (router, handler) = oneshot::channel();
        let status_server = server.clone();
        tokio::spawn(async move {
            let _ = router.send(());
            loop {
                {
                    let online = status_server.workers.read().await;
                    let online_workers: Vec<&String> = online.keys().collect();
                    info!("online workers: {}", online_workers.len(),);
                    debug!("online workers: {:?}", online_workers);
                    info!("tasks in queue: {}", status_server.queue.read().await.len());
                }
                let _ = sleep(Duration::from_secs(30)).await;
            }
        });
        let _ = handler.await;

        Ok(())
    }
}

pub async fn start_proxy(
    tcp: SocketAddr,
    rest: SocketAddr,
    redis: &str,
    network: String,
) -> Result<()> {
    let server = ProxyServer::new(redis);
    let listener = TcpListener::bind(&tcp).await.unwrap();
    let (router, handler) = oneshot::channel();
    let server_clone = server.clone();
    let proxy_handler = tokio::spawn(async move {
        let _ = router.send(());
        loop {
            match listener.accept().await {
                Ok((stream, ip)) => {
                    debug!("new connection from {}", ip);
                    if let Err(e) =
                        ProxyServer::handle_stream(stream, server_clone.clone(), network.clone())
                            .await
                    {
                        log::error!("failed to handle stream, {e}");
                    }
                }
                Err(e) => log::error!("failed to accept connection, {:?}", e),
            }
        }
    });
    let _ = handler.await;

    let rest_handler = tokio::spawn(async move {
        let _ = start_rest(server.clone(), rest).await;
    });
    let _ = tokio::join!(proxy_handler, rest_handler);
    std::future::pending::<()>().await;
    Ok(())
}

pub async fn start_rest(shared: Arc<ProxyServer>, rest: SocketAddr) -> Result<()> {
    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/submit", post(submit_task_handler))
        .route("/task", post(task_result_handler))
        .with_state(shared)
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|_: BoxError| async {
                    StatusCode::REQUEST_TIMEOUT
                }))
                .layer(TimeoutLayer::new(Duration::from_secs(60))),
        )
        .layer(
            CorsLayer::new()
                .allow_methods(Any)
                .allow_origin(Any)
                .allow_headers(Any),
        );
    let listener = TcpListener::bind(&rest).await?;
    info!("rest server listening on {}", &rest);
    axum::serve(listener, router).await?;
    Ok(())
}

pub async fn health_handler() -> Json<bool> {
    Json(true)
}

pub async fn submit_task_handler(
    State(shared): State<Arc<ProxyServer>>,
    extract::Json(request): extract::Json<TaskSubmit>,
) -> Json<TaskSubmitResponse> {
    let id = format!(
        "{}-{}-{}",
        hex::encode(&request.node_id),
        request.start_nonce,
        request.end_nonce
    );
    if shared
        .queue
        .read()
        .await
        .iter()
        .filter(|&(task, _)| task.node_id == request.node_id)
        .collect::<Vec<(&TaskSubmit, &Reverse<u16>)>>()
        .len()
        > 0
        || shared.db.read().await.get_str(&id).is_ok()
        || shared.pending.read().await.get(&id).is_some()
    {
        warn!("duplicated task, ignore");
        return Json(TaskSubmitResponse { id });
    }
    debug!("new task submitted {}", id);
    for (k, v) in shared.workers.read().await.iter() {
        if v.status == 1 {
            if let Err(e) = v
                .router
                .send(ServerMessage::ServerSubmitRequest(ServerSubmitRequest {
                    worker_name: Some(k.to_string()),
                    request: request,
                }))
                .await
            {
                log::error!("failed to send task to manager {}", e);
            }
            return Json(TaskSubmitResponse { id });
        }
    }
    let _ = shared
        .queue
        .write()
        .await
        .push(request.clone(), Reverse(request.priority));
    debug!("task added to queue {}", id);
    Json(TaskSubmitResponse { id })
}

pub async fn task_result_handler(
    State(shared): State<Arc<ProxyServer>>,
    extract::Json(request): extract::Json<TaskSubmitResponse>,
) -> Json<TaskResponse> {
    let task_id = request.id;
    let completed_result = shared.db.read().await.get_str(&task_id);
    if shared
        .queue
        .read()
        .await
        .iter()
        .filter(|&(task, _)| hex::encode(task.node_id) == task_id)
        .collect::<Vec<(&TaskSubmit, &Reverse<u16>)>>()
        .len()
        == 0
        && completed_result.is_err()
        && shared.pending.read().await.get(&task_id).is_none()
    {
        warn!("task missed in queue and completed db");
        return Json(TaskResponse::missed(task_id));
    }
    match completed_result {
        Ok(data) => {
            let data: TaskResponse = serde_json::from_str(&data).unwrap();
            Json(data)
        }
        Err(_) => Json(TaskResponse::init(task_id)),
    }
}
