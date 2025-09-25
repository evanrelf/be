pub async fn flatten<T, E1, E2>(
    future: impl Future<Output = Result<Result<T, E2>, E1>>,
) -> Result<T, E2>
where
    E1: std::error::Error,
    E2: From<E1>,
{
    future.await?
}
