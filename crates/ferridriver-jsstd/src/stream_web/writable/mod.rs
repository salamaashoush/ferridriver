mod default_controller;
mod default_writer;
mod objects;
mod stream;
mod writer;

pub(crate) use default_controller::{
    WritableAbortAlgorithm, WritableCloseAlgorithm, WritableStartAlgorithm,
    WritableStreamDefaultController, WritableStreamDefaultControllerPrimordials,
    WritableWriteAlgorithm,
};
pub(crate) use default_writer::{WritableStreamDefaultWriter, WritableStreamDefaultWriterOwned};
pub(crate) use objects::{WritableStreamClassObjects, WritableStreamObjects};
pub(crate) use stream::{
    WritableStream, WritableStreamClass, WritableStreamOwned, WritableStreamState,
};

/// ! WritableStreamDefaultControllerErrorIfNeeded(stream.[[controller]], e)
/// reached from a stream class rather than from live `WritableStreamObjects`.
///
/// TransformStreamErrorWritableAndUnblockWrite needs this step; without it
/// an errored transform leaves its writable in the "writable" state with
/// an unresolved write request, which strands the promise (and everything
/// it retains) until the runtime is torn down.
pub(crate) fn writable_stream_error_if_needed<'js>(
    ctx: rquickjs::Ctx<'js>,
    stream_class: WritableStreamClass<'js>,
    error: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    let objects =
        WritableStreamObjects::from_stream(rquickjs::class::OwnedBorrowMut::from_class(stream_class))
            .refresh_writer();
    if !matches!(objects.stream.state, WritableStreamState::Writable) {
        return Ok(());
    }
    WritableStreamDefaultController::writable_stream_default_controller_error(ctx, objects, error)?;
    Ok(())
}
