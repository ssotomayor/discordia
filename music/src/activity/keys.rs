use discordia_bot::{
    BotIdentity,
    activity_audio::{AudioKey, KeyContext},
    protocol::{ActivityInstance, Id, activity_audio_epoch},
};

struct Publication {
    id: Id,
    epoch: String,
    key: AudioKey,
}

pub struct PublicationKeys {
    instance: ActivityInstance,
    publication: Option<Publication>,
}

impl PublicationKeys {
    pub fn new(instance: ActivityInstance) -> Result<Self, &'static str> {
        if instance.participants.is_empty() {
            return Err("the music session has ended");
        }
        let publication = Some(Self::fresh(&instance));
        Ok(Self {
            instance,
            publication,
        })
    }

    fn fresh(instance: &ActivityInstance) -> Publication {
        Publication {
            id: Id::new_v4(),
            epoch: activity_audio_epoch(instance),
            key: AudioKey::generate(),
        }
    }

    // Callers must close the old publication before publishing with this key;
    // an expired grant alone does not disconnect an already connected listener.
    pub fn sync(&mut self, instance: ActivityInstance) -> Result<bool, &'static str> {
        if instance.id != self.instance.id
            || instance.channel_id != self.instance.channel_id
            || instance.app != self.instance.app
        {
            self.publication = None;
            return Err("the music session scope changed");
        }
        let changed = self
            .publication
            .as_ref()
            .is_none_or(|publication| publication.epoch != activity_audio_epoch(&instance));
        self.instance = instance;
        if self.instance.participants.is_empty() {
            self.publication = None;
            return Err("the music session has ended");
        }
        if changed {
            self.publication = Some(Self::fresh(&self.instance));
        }
        Ok(changed)
    }

    pub fn retire(&mut self) {
        self.publication = None;
    }

    pub fn seal(
        &self,
        identity: &BotIdentity,
        recipient: &str,
        request: Id,
        epoch: &str,
    ) -> Result<(KeyContext, String), &'static str> {
        let publication = self.publication.as_ref().ok_or("music audio is stopped")?;
        if identity.pubkey() != self.instance.app
            || self.instance.role_of(recipient).is_none()
            || epoch != publication.epoch
        {
            return Err("you cannot receive this music key");
        }
        let mut epoch_bytes = [0; 16];
        hex::decode_to_slice(epoch, &mut epoch_bytes).map_err(|_| "invalid music scope")?;
        let context = KeyContext {
            instance: *self.instance.id.as_bytes(),
            channel: *self.instance.channel_id.as_bytes(),
            epoch: epoch_bytes,
            request: *request.as_bytes(),
            publication: *publication.id.as_bytes(),
        };
        let ciphertext = identity.seal_activity_audio_key(recipient, &context, &publication.key)?;
        Ok((context, ciphertext))
    }
}
