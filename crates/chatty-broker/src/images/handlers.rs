//! Image generation requests.
use super::*;

pub(crate) async fn generate(
    ctx: &Ctx,
    session_token: &str,
    prompt: String,
    negative_prompt: String,
    square: bool,
    seed: Option<u64>,
) -> Result<()> {
    auth(ctx.db(), session_token).await?;
    let data = ctx
        .image_job(generate_png(
            &ctx.app,
            &prompt,
            &negative_prompt,
            square,
            seed,
        ))
        .await?;
    ctx.respond(Response::GeneratedImage { data }).await
}

pub(crate) async fn scene(
    ctx: &Ctx,
    session_token: &str,
    conversation_id: String,
    prompt: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    own_conversation(ctx.db(), &uid, &conversation_id).await?;
    validate_image_prompt(&prompt)?;
    let work = async {
        let prompt = match prompt.trim() {
            "" => describe_scene(&ctx.app, &uid, &conversation_id).await?,
            prompt => prompt.to_owned(),
        };
        let png = generate_png(&ctx.app, &prompt, "", false, None).await?;
        Ok((prompt, png))
    };
    let (prompt, png) = ctx.image_job(work).await?;
    let (message_id, rev, changed) =
        save_scene_image(&ctx.app, &uid, &conversation_id, &prompt, &png).await?;
    ctx.delta(
        &uid,
        rev,
        "message",
        &message_id,
        DeltaOperation::Add,
        changed,
    )
    .await?;
    ctx.accepted(Some(message_id), rev).await
}

pub(crate) async fn message_image(
    ctx: &Ctx,
    session_token: &str,
    message_id: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let data = load_message_image(&ctx.app, &uid, &message_id).await?;
    ctx.respond(Response::MessageImage { message_id, data })
        .await
}
