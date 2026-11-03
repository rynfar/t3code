//! A successful upload is released unless the composer adopts its asset id.

pub struct UploadedAttachment {
    id: Option<String>,
    release: Option<Box<dyn FnOnce(String) + Send>>,
}

impl UploadedAttachment {
    pub(super) fn new(id: String, release: impl FnOnce(String) + Send + 'static) -> Self {
        Self { id: Some(id), release: Some(Box::new(release)) }
    }

    pub fn into_id(mut self) -> String {
        self.release = None;
        self.id.take().unwrap()
    }
}

impl Drop for UploadedAttachment {
    fn drop(&mut self) {
        if let (Some(id), Some(release)) = (self.id.take(), self.release.take()) {
            release(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn a_cancelled_waiter_releases_the_completed_asset_but_adoption_keeps_it() {
        let released = Arc::new(Mutex::new(Vec::new()));
        let lease = |id: &str| {
            let released = released.clone();
            UploadedAttachment::new(id.into(), move |id| released.lock().unwrap().push(id))
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let upload = lease("pending-discard");
        drop(receiver);
        let finished = tokio::spawn(async move {
            drop(sender.send(upload));
        });
        finished.await.unwrap();
        assert_eq!(*released.lock().unwrap(), ["pending-discard"], "discarding the waiter must not orphan the Tokio upload result");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(sender.send(lease("unread-result")).is_ok());
        drop(receiver);
        assert_eq!(*released.lock().unwrap(), ["pending-discard", "unread-result"]);
        assert_eq!(lease("adopted").into_id(), "adopted");
        assert_eq!(released.lock().unwrap().len(), 2);
    }
}
