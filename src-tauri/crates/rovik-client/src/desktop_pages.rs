//! One-shot page requests belong to exactly one live UI callback, never the journal.
use crate::{desktop_journal::Scope, desktop_session::Session, PageToOpen};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

struct Target {
    id: String,
    owner: Weak<Session>,
    sink: String,
    scope: Scope,
}
/// Private page callbacks registered by connected windows.
#[derive(Default)]
pub struct PageRouter(Mutex<HashMap<String, Target>>);

/// A page carries its forwarding metadata because the journal arrives independently.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageEvent<'a> {
    dialog_id: &'a str,
    lead_id: &'a str,
    project_id: Option<&'a str>,
    address: &'a str,
    in_tab: bool,
    ports: &'a [crate::ForwardedPort],
}
impl PageRouter {
    pub(crate) fn subscribe(
        self: &Arc<Self>,
        owner: &Arc<Session>,
        sink: String,
        scope: Scope,
    ) -> Result<String, String> {
        if scope.chats.len() > 256 {
            return Err("Too many chats in page callback scope".into());
        }
        owner.begin(&sink)?;
        let id = uuid::Uuid::new_v4().to_string();
        let old = self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(
            owner.id().into(),
            Target {
                id: id.clone(),
                owner: Arc::downgrade(owner),
                sink: sink.clone(),
                scope,
            },
        );
        if let Some(old) = old {
            if let Some(owner) = old.owner.upgrade() {
                owner.remove(&format!("pages:{}", old.id));
            }
        }
        let router = self.clone();
        let client = owner.id().to_string();
        let generation = id.clone();
        if let Err(error) = owner.register(format!("pages:{id}"), sink.clone(), move || {
            let mut targets = router.0.lock().unwrap_or_else(|e| e.into_inner());
            if targets
                .get(&client)
                .is_some_and(|target| target.id == generation)
            {
                targets.remove(&client);
            }
        }) {
            owner.cancel_sink(&sink);
            return Err(error);
        }
        Ok(id)
    }

    /// Delivers once to the project's window, or a unique retained-chat owner.
    /// Missing and ambiguous targets fail visibly through the remote's existing notice.
    pub fn open(&self, page: PageToOpen<'_>) -> Result<(), String> {
        let targets = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let candidates: Vec<_> = targets
            .values()
            .filter_map(|target| {
                let owner = target.owner.upgrade().filter(|owner| !owner.is_closed())?;
                let project = page.project_id.is_some()
                    && target.scope.project_id.as_deref() == page.project_id;
                let retained = target
                    .scope
                    .chats
                    .iter()
                    .any(|chat| chat == page.dialog_id || chat == page.lead_id);
                let unassigned = page.project_id.is_none() && target.scope.project_id.is_none();
                let rank = if project {
                    3
                } else if retained {
                    2
                } else if unassigned {
                    1
                } else {
                    return None;
                };
                Some((rank, target, owner))
            })
            .collect();
        let best = candidates.iter().map(|(rank, _, _)| *rank).max().ok_or(
            "No window is following this chat; open its project to use the forwarded page",
        )?;
        let mut chosen = candidates.iter().filter(|(rank, _, _)| *rank == best);
        let (_, target, owner) = chosen.next().ok_or("No page callback is available")?;
        if chosen.next().is_some() {
            return Err("More than one window is following this chat; open the forwarded page from its chat instead".into());
        }
        if !owner.emit(
            &target.sink,
            PageEvent {
                dialog_id: page.dialog_id,
                lead_id: page.lead_id,
                project_id: page.project_id,
                address: page.address,
                in_tab: page.in_tab,
                ports: page.ports,
            },
        ) {
            return Err("The page's window disconnected or stopped accepting events".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_session::{EventBody, Sessions};
    fn scope(project: Option<&str>, chats: &[&str]) -> Scope {
        Scope {
            project_id: project.map(str::to_string),
            chats: chats.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn page() -> PageToOpen<'static> {
        PageToOpen {
            dialog_id: "child",
            lead_id: "lead",
            project_id: Some("one"),
            address: "http://localhost:3000/",
            in_tab: true,
            ports: &[],
        }
    }
    #[tokio::test]
    async fn project_window_receives_page_and_ports_in_one_private_event() {
        let router = Arc::new(PageRouter::default());
        let sessions = Sessions::default();
        let one = sessions.create();
        let two = sessions.create();
        let mut events = one.session.events().unwrap();
        router
            .subscribe(&one.session, "one".into(), scope(Some("one"), &[]))
            .unwrap();
        router
            .subscribe(&two.session, "two".into(), scope(Some("two"), &["lead"]))
            .unwrap();
        let ports = [crate::ForwardedPort {
            port: 3000,
            local_port: 4000,
            tab_port: Some(5000),
        }];
        router
            .open(PageToOpen {
                ports: &ports,
                ..page()
            })
            .unwrap();
        let event = events.next().await.unwrap();
        assert_eq!(event.sink, "one");
        let EventBody::Data(data) = event.body else {
            panic!("page expected")
        };
        assert_eq!(data["dialogId"], "child");
        assert_eq!(data["ports"][0]["tabPort"], 5000);
        drop(one);
        router.open(page()).unwrap(); // Retained lead is the fallback.
        drop(two);
        assert!(router.open(page()).unwrap_err().contains("No window"));
    }
    #[test]
    fn ambiguous_targets_fail_and_stale_or_foreign_unsubscribe_cannot_remove_replacement() {
        let router = Arc::new(PageRouter::default());
        let sessions = Sessions::default();
        let one = sessions.create();
        let two = sessions.create();
        let old = router
            .subscribe(&one.session, "old".into(), scope(Some("one"), &[]))
            .unwrap();
        let new = router
            .subscribe(&one.session, "new".into(), scope(Some("one"), &[]))
            .unwrap();
        assert!(!one.session.remove(&format!("pages:{old}")));
        assert!(!two.session.remove(&format!("pages:{new}")));
        router.open(page()).unwrap();
        router
            .subscribe(&two.session, "two".into(), scope(Some("one"), &[]))
            .unwrap();
        assert!(router.open(page()).unwrap_err().contains("More than one"));
        one.session.remove(&format!("pages:{new}"));
        router.open(page()).unwrap();
        two.session.cancel_sink("two");
        assert!(router.open(page()).is_err());
    }
    #[test]
    fn no_project_pages_prefer_retained_chats_and_closed_sessions_cannot_subscribe() {
        let router = Arc::new(PageRouter::default());
        let sessions = Sessions::default();
        let one = sessions.create();
        let two = sessions.create();
        router
            .subscribe(&one.session, "one".into(), scope(None, &[]))
            .unwrap();
        router
            .subscribe(&two.session, "two".into(), scope(None, &[]))
            .unwrap();
        let page = PageToOpen {
            project_id: None,
            ..page()
        };
        assert!(router.open(page).is_err());
        router
            .subscribe(
                &two.session,
                "retained".into(),
                scope(Some("two"), &["child"]),
            )
            .unwrap();
        router.open(page).unwrap();
        two.session.close("gone");
        assert!(router
            .subscribe(&two.session, "late".into(), scope(None, &[]))
            .is_err());
        router.open(page).unwrap();
    }
}
