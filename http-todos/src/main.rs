#![recursion_limit = "256"]

use http_gateway::{
    handler::{Json, Json201, ResourceLocation, Response, StringId},
    http_server_main,
    hyper::StatusCode,
    router::{MakeRoute, Router, RouterHandler, ext::RouterExt},
    serde_json,
};
use std::{borrow::Cow, cell::RefCell, collections::BTreeMap, convert::Infallible, rc::Rc};
use uuid::Uuid;

fn main() {
    let db = [(
        uuid::uuid!("019d6b16-a252-759c-b209-d102bcb1d0da"),
        Rc::new(RefCell::new(TodoTask {
            title: "Make a todo API".to_string(),
            done: false,
        })),
    )]
    .into_iter()
    .collect();

    http_server_main(|| {
        RouterHandler::new(ApiRoot {
            db: Rc::new(RefCell::new(db)),
        })
    });
}

type TasksDb = Rc<RefCell<BTreeMap<Uuid, Rc<RefCell<TodoTask>>>>>;

#[derive(Clone)]
struct ApiRoot {
    db: TasksDb,
}
impl MakeRoute for ApiRoot {
    async fn register<R: Router<Self>>(router: &mut R) {
        router
            .path("tasks", async |self_, _| TasksDbRoute { db: self_.db })
            .await;
    }
}

struct TasksDbRoute {
    db: TasksDb,
}
impl TasksDbRoute {
    fn id(path: &StringId) -> Uuid {
        path.id().parse().unwrap_or_default()
    }
}
impl MakeRoute for TasksDbRoute {
    async fn register<R: Router<Self>>(router: &mut R) {
        router
            .get(async |self_, _| {
                let tasks = self_
                    .db
                    .borrow()
                    .iter()
                    .map(|(id, task)| TodoTaskOut::from((*id, task.borrow().clone().clone())))
                    .collect::<Vec<_>>();
                Json::j200(tasks)
            })
            .await;

        router
            .delete_route(async |self_, _, path| {
                let id = Self::id(&path);
                let task = self_.db.borrow_mut().remove(&id)?.clone();

                Some(Json::j200(TodoTaskOut::from((id, task.borrow().clone()))))
            })
            .await;

        router
            .post_body(async |self_, new_task: NewTask, _| {
                let id = Uuid::now_v7();
                let new_task = TodoTask::from(new_task);
                self_
                    .db
                    .borrow_mut()
                    .insert(id, Rc::new(RefCell::new(new_task.clone())));

                // TODO 201 with location
                Result::<_, Error422>::Ok(Json201(TodoTaskOut::from((id, new_task))))
            })
            .await;

        router
            .route(async |self_, _, path| {
                let id = Self::id(&path);
                let task = self_.db.borrow().get(&id)?.clone();

                Some(ExistentTask { id, task })
            })
            .await;
    }
}

#[derive(Clone)]
struct TodoTask {
    pub title: String,
    pub done: bool,
}
impl From<NewTask> for TodoTask {
    fn from(value: NewTask) -> Self {
        Self {
            title: value.title,
            done: false,
        }
    }
}

struct ExistentTask {
    id: Uuid,
    task: Rc<RefCell<TodoTask>>,
}
impl MakeRoute for ExistentTask {
    async fn register<R: Router<Self>>(router: &mut R) {
        router
            .get(async |self_, _| {
                Json::j200(TodoTaskOut::from((self_.id, self_.task.borrow().clone())))
            })
            .await;

        router
            .attribute::<Infallible, _, _, _, _>(
                "title",
                async |self_, title| {
                    self_.task.borrow_mut().title = title;
                    Ok(())
                },
                async |self_| Ok(self_.task.borrow().title.clone()),
            )
            .await;

        router
            .attribute::<Infallible, _, _, _, _>(
                "done",
                async |self_, done| {
                    self_.task.borrow_mut().done = done;
                    Ok(())
                },
                async |self_| Ok(self_.task.borrow().done),
            )
            .await;
    }
}

#[derive(serde::Serialize)]
struct TodoTaskOut {
    pub id: Uuid,
    pub title: String,
    pub done: bool,
}
impl ResourceLocation for TodoTaskOut {
    fn base() -> &'static str {
        "/tasks/"
    }

    fn resource_id(&self) -> Cow<'_, str> {
        Cow::Owned(self.id.to_string())
    }
}
impl From<(Uuid, TodoTask)> for TodoTaskOut {
    fn from((id, TodoTask { title, done }): (Uuid, TodoTask)) -> Self {
        Self { id, title, done }
    }
}

#[derive(serde::Deserialize)]
pub struct NewTask {
    pub title: String,
}

#[derive(serde::Serialize)]
struct Error422 {
    error: String,
}
impl From<serde_json::Error> for Error422 {
    fn from(value: serde_json::Error) -> Self {
        Self {
            error: value.to_string(),
        }
    }
}
impl Response for Error422 {
    type Body = <Json<Self> as Response>::Body;

    fn into_body(self) -> Self::Body {
        let code = self.status_code();
        Json(self, code).into_body()
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::UNPROCESSABLE_ENTITY
    }
}
