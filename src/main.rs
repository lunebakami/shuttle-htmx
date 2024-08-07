use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::Event, IntoResponse, Response, Sse},
    routing::{delete, get},
    Extension, Form, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{migrate, FromRow, PgPool};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast::{channel, Receiver, Sender};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt as _};

#[derive(Clone, Serialize, Debug)]
enum MutationKind {
    Create,
    Delete,
}

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
}

#[derive(Clone, Serialize, Debug)]
pub struct TodoUpdate {
    mutation_kind: MutationKind,
    id: i32,
}

#[derive(FromRow, Serialize, Deserialize)]
pub struct Todo {
    pub id: i32,
    pub description: String,
}

#[derive(FromRow, Serialize, Deserialize)]
pub struct TodoNew {
    pub description: String,
}

#[derive(Template)]
#[template(path = "todo.html")]
pub struct TodoNewTemplate {
    pub todo: Todo,
}

#[derive(Template)]
#[template(path = "todos.html")]
pub struct Records {
    pub todos: Vec<Todo>,
}

type TodosStream = Sender<TodoUpdate>;

async fn create_todo(
    State(state): State<AppState>,
    Extension(tx): Extension<TodosStream>,
    Form(form): Form<TodoNew>,
) -> impl IntoResponse {
    let todo = sqlx::query_as::<_, Todo>(
        "INSERT INTO TODOS (description) VALUES ($1) RETURNING id, description",
    )
    .bind(form.description)
    .fetch_one(&state.db)
    .await
    .unwrap();

    if let Err(e) = tx.send(TodoUpdate {
        mutation_kind: MutationKind::Create,
        id: todo.id,
    }) {
        eprintln!(
            "Tried to send log of record with ID {} created but something went wrong: {e}",
            todo.id
        );
    }

    TodoNewTemplate { todo }
}

async fn delete_todo(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Extension(tx): Extension<TodosStream>,
) -> impl IntoResponse {
    sqlx::query("DELETE FROM TODOS WHERE ID = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    if let Err(e) = tx.send(TodoUpdate {
        mutation_kind: MutationKind::Delete,
        id,
    }) {
        eprintln!("Tried to send log of record with ID {id} created but something went wrong: {e}",);
    }

    StatusCode::OK
}

pub async fn handle_stream(
    Extension(tx): Extension<TodosStream>,
    Extension(rx): Extension<Receiver<TodoUpdate>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(rx);

    // map the stream to axum Events which get sent through the SSE stream
    Sse::new(
        stream
            .map(|msg| {
                let msg = msg.unwrap();

                let json = format!("<div>{}</div>", json!(msg));
                Event::default().data(json)
            })
            .map(Ok),
    )
    .keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(600))
            .text("keep-alive-text"),
    )
}

#[shuttle_runtime::main]
async fn main(#[shuttle_shared_db::Postgres] db: PgPool) -> shuttle_axum::ShuttleAxum {
    migrate!()
        .run(&db)
        .await
        .expect("Looks like something went wrong with migrations :(");

    let (tx, rx) = channel::<TodoUpdate>(10);
    let state = AppState { db };

    let router = Router::new()
        .route("/", get(home))
        .route("/stream", get(stream))
        .route("/todos", get(fetch_todos).post(create_todo))
        .route("/todos/:id", delete(delete_todo))
        // new handler - we will make this later
        .route("/todos/stream", get(handle_stream))
        .with_state(state)
        .layer(Extension(tx))
        .layer(Extension(rx));

    Ok(router.into())
}
