use async_trait::async_trait;
use cqrs_es::{Aggregate, DomainEvent};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
}

#[async_trait]
impl Aggregate for Journey {
    type Command = JourneyCommand;
    type Event = JourneyEvent;
    type Error = JourneyError;
    type Services = JourneyServices;

    // This identifier should be unique to the system.
    fn aggregate_type() -> String {
        "Journey".to_string()
    }

    // The aggregate logic goes here. Note that this will be the _bulk_ of a CQRS system
    // so expect to use helper functions elsewhere to keep the code clean.
    async fn handle(
        &self,
        command: Self::Command,
        _services: &Self::Services,
    ) -> Result<Vec<Self::Event>, Self::Error> {
        match command {
            JourneyCommand::Start { id } => {
                if self.id == id {
                    Err(JourneyError::AlreadyStarted)
                } else {
                    Ok(vec![JourneyEvent::Started { id }])
                }
            }
            JourneyCommand::Modify => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    Ok(vec![JourneyEvent::Modified])
                }
            }
            JourneyCommand::Complete => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    Ok(vec![JourneyEvent::Completed])
                }
            }
        }
    }

    fn apply(&mut self, event: Self::Event) {
        match event {
            JourneyEvent::Started { id } => {
                self.id = id;
                self.state = JourneyState::InProgress;
            }
            JourneyEvent::Modified => {}
            JourneyEvent::Completed => {
                self.state = JourneyState::Complete;
            }
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub enum JourneyState {
    #[default]
    InProgress,
    Complete,
}

#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    Start { id: Uuid },
    Modify,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JourneyEvent {
    Started { id: Uuid },
    Modified,
    Completed,
}

impl DomainEvent for JourneyEvent {
    fn event_type(&self) -> String {
        let event_type: &str = match self {
            JourneyEvent::Started { .. } => "JourneyOpened",
            JourneyEvent::Modified => "JourneyModified",
            JourneyEvent::Completed => "JourneyClosed",
        };
        event_type.to_string()
    }

    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum JourneyError {
    #[error("Journey not found")]
    NotFound,
    #[error("Journey already opened")]
    AlreadyStarted,
    #[error("Journey already closed")]
    AlreadyCompleted,
}

pub struct JourneyServices;

impl JourneyServices {
    #[allow(dead_code, clippy::unused_async)]
    async fn do_something(&self) -> Result<(), JourneyError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use cqrs_es::{AggregateError, CqrsFramework, EventStore, mem_store::MemStore};
    use uuid::Uuid;

    use super::*;
    use crate::SimpleLoggingQuery;

    #[tokio::test]
    async fn happy_path() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let cqrs = CqrsFramework::new(event_store.clone(), vec![Box::new(query)], JourneyServices);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // modify the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Modify)
            .await
            .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // this here to show how to list events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("{events:#?}");
    }

    #[tokio::test]
    async fn open_already_opened() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], JourneyServices);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // try to start the Journey again
        let result = cqrs
            .execute(&id.to_string(), JourneyCommand::Start { id })
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::AlreadyStarted))
        ));
    }

    #[tokio::test]
    async fn complete_not_started() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], JourneyServices);

        let id = Uuid::new_v4();

        // try to complete the Journey
        let result = cqrs
            .execute(&id.to_string(), JourneyCommand::Complete)
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::NotFound))
        ));
    }

    #[tokio::test]
    async fn complete_already_completed() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], JourneyServices);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // try to complete the Journey again
        let result = cqrs
            .execute(&id.to_string(), JourneyCommand::Complete)
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::AlreadyCompleted))
        ));
    }

    #[tokio::test]
    async fn modify_not_started() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], JourneyServices);

        let id = Uuid::new_v4();

        // try to modify the Journey before starting
        let result = cqrs.execute(&id.to_string(), JourneyCommand::Modify).await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::NotFound))
        ));
    }

    #[tokio::test]
    async fn modify_already_completed() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], JourneyServices);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // try to modify the Journey after completion
        let result = cqrs.execute(&id.to_string(), JourneyCommand::Modify).await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::AlreadyCompleted))
        ));
    }
}
