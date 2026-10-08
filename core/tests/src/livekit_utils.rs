use livekit_api::access_token;

/// Room every test participant joins.
pub const ROOM: &str = "dev_room";

pub fn generate_token(name: &str) -> String {
    generate_token_with_identity(name, name)
}

/// Token for `user`'s `track` connection ("audio" or "video"), with an identity in the format
/// the backend issues: `room:<room>:<user id>:<track>`. Core only lists participants with this
/// format in its `ParticipantsSnapshot`.
pub fn generate_participant_token(user: &str, track: &str) -> String {
    generate_token_with_identity(&participant_identity(user, track), user)
}

/// `room:<room>:<user id>:<track>`, the user id being `user` lowercased with separators replaced.
pub fn participant_identity(user: &str, track: &str) -> String {
    format!("{}:{track}", participant_base_identity(user))
}

/// The `room:<room>:<user id>` prefix shared by a participant's audio and video track identities,
/// the user id being `user` lowercased with separators replaced.
pub fn participant_base_identity(user: &str) -> String {
    let user_id: String = user
        .chars()
        .map(|c| {
            if c == ':' || c.is_whitespace() {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    format!("room:{ROOM}:{user_id}")
}

fn generate_token_with_identity(identity: &str, name: &str) -> String {
    let api_key = std::env::var("LIVEKIT_API_KEY").unwrap();
    let api_secret = std::env::var("LIVEKIT_API_SECRET").unwrap();

    access_token::AccessToken::with_api_key(&api_key, &api_secret)
        .with_identity(identity)
        .with_name(name)
        .with_grants(access_token::VideoGrants {
            room_join: true,
            room: ROOM.to_string(),
            ..Default::default()
        })
        .to_jwt()
        .unwrap()
}
