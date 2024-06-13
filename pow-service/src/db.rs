use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sqlx::{migrate::MigrateDatabase, FromRow, Pool, Row, Sqlite, SqlitePool};

use crate::messages::TaskResponse;

#[derive(Debug, Clone, PartialEq, FromRow)]
struct DBPowResponse {
    pub id: String,
    pub status: u8,
    pub data: String,
    pub ts: Option<DateTime<Utc>>,
}

impl DBPowResponse {
    fn new(task_response: TaskResponse) -> Self {
        let TaskResponse { id, status, data } = task_response;
        let id = format!("{:*<80}", id);
        Self {
            id,
            status,
            data: serde_json::to_string(&data).unwrap(),
            ts: None,
        }
    }

    fn to_task_response(self) -> TaskResponse {
        let DBPowResponse {
            id,
            status,
            data,
            ts: _,
        } = self;
        TaskResponse {
            id,
            status,
            data: serde_json::from_str(&data).unwrap(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DBHandler {
    pub pool: Pool<Sqlite>,
}

impl DBHandler {
    pub async fn new(datadir: PathBuf) -> Self {
        let db_url = datadir.join("pow_service.sql");
        let db_url = db_url.to_str().unwrap();
        if !Sqlite::database_exists(db_url).await.unwrap() {
            Sqlite::create_database(db_url).await.unwrap();
        }
        let db = SqlitePool::connect(db_url).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        Self { pool: db }
    }

    pub async fn insert_one(&self, task_result: TaskResponse) -> Result<String, sqlx::Error> {
        let DBPowResponse {
            id,
            status,
            data,
            ts: _,
        } = DBPowResponse::new(task_result);
        let result = sqlx::query(
            "INSERT INTO pow_service (id, status, data) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(id)
        .bind(status)
        .bind(data)
        .fetch_one(&self.pool)
        .await?
        .get(0);
        Ok(result)
    }

    pub async fn get_one(&self, id: String) -> Result<TaskResponse, sqlx::Error> {
        let id = format!("{:*<80}", id);
        let result = sqlx::query_as::<_, DBPowResponse>("SELECT * FROM pow_service WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(result.to_task_response())
    }

    pub async fn expire_records(&self, deadline: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let _ = sqlx::query("DELETE FROM pow_service WHERE ts < $1")
            .bind(deadline)
            .fetch_all(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use chrono::Duration;
    use sqlx::types::chrono::Utc;

    use crate::{db::DBHandler, messages::TaskResponse};

    #[tokio::test]
    async fn sqlite_should_work() {
        let db_handler = DBHandler::new(PathBuf::from("../")).await;
        let mut data = HashMap::new();
        data.insert(1, 2);
        let task_response = TaskResponse {
            id: "testid".to_string(),
            status: 3,
            data: Some(data),
        };
        let saved = db_handler.insert_one(task_response).await;
        println!("saved: {:?}", saved);

        let db_res = db_handler.get_one("testid".to_string()).await;
        println!("db_res: {:?}", db_res);

        let now = Utc::now();
        let deadline = now - Duration::days(7);
        let result = db_handler.expire_records(deadline).await;
        println!("expired: {:?}", result);

        let db_res = db_handler.get_one("testid".to_string()).await;
        println!("db_res: {:?}", db_res);

        let _ = db_handler.expire_records(Utc::now()).await.unwrap();
    }
}
