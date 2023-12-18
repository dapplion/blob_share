use ethers::{
    providers::{Http, Middleware, Provider, ProviderError, StreamExt, Ws},
    types::{Block, BlockId, Bytes, NameOrAddress, Transaction, TxHash, H256, U256, U64},
};
use eyre::{Context, Result};
use futures::stream::Stream;
use std::pin::Pin;
use url::Url;

pub enum EthProvider {
    Http(Provider<Http>),
    Ws(Provider<Ws>),
}

impl EthProvider {
    pub async fn new(http_or_ws_url: &str) -> Result<Self> {
        Ok(
            match Url::parse(http_or_ws_url)
                .wrap_err_with(|| format!("invalid eth provider URL {}", http_or_ws_url))?
                .scheme()
            {
                "ws" | "wss" => EthProvider::new_ws(http_or_ws_url)
                    .await
                    .wrap_err_with(|| {
                        format!("unable to connect to WS eth provider {}", http_or_ws_url)
                    })?,
                _ => EthProvider::new_http(http_or_ws_url)?,
            },
        )
    }

    pub fn new_http(http_url: &str) -> Result<Self> {
        Ok(Self::Http(Provider::<Http>::try_from(http_url)?))
    }

    pub async fn new_ws(ws_url: &str) -> Result<Self> {
        Ok(Self::Ws(Provider::<Ws>::connect(ws_url).await?))
    }

    pub async fn get_chainid(&self) -> Result<U256, ProviderError> {
        match self {
            EthProvider::Http(provider) => provider.get_chainid().await,
            EthProvider::Ws(provider) => provider.get_chainid().await,
        }
    }

    pub async fn get_block_number(&self) -> Result<U64, ProviderError> {
        match self {
            EthProvider::Http(provider) => provider.get_block_number().await,
            EthProvider::Ws(provider) => provider.get_block_number().await,
        }
    }

    pub async fn get_transaction_count<T: Into<NameOrAddress> + Send + Sync>(
        &self,
        from: T,
        block: Option<BlockId>,
    ) -> Result<U256, ProviderError> {
        match self {
            EthProvider::Http(provider) => provider.get_transaction_count(from, block).await,
            EthProvider::Ws(provider) => provider.get_transaction_count(from, block).await,
        }
    }

    pub async fn get_block<T: Into<BlockId> + Send + Sync>(
        &self,
        block_hash_or_number: T,
    ) -> Result<Option<Block<TxHash>>, ProviderError> {
        match self {
            EthProvider::Http(provider) => provider.get_block(block_hash_or_number).await,
            EthProvider::Ws(provider) => provider.get_block(block_hash_or_number).await,
        }
    }

    pub async fn get_block_with_txs<T: Into<BlockId> + Send + Sync>(
        &self,
        block_hash_or_number: T,
    ) -> Result<Option<Block<Transaction>>, ProviderError> {
        match self {
            EthProvider::Http(provider) => provider.get_block_with_txs(block_hash_or_number).await,
            EthProvider::Ws(provider) => provider.get_block_with_txs(block_hash_or_number).await,
        }
    }

    pub async fn send_raw_transaction(&self, tx: Bytes) -> Result<TxHash, ProviderError> {
        match self {
            EthProvider::Http(provider) => Ok(provider.send_raw_transaction(tx).await?.tx_hash()),
            EthProvider::Ws(provider) => Ok(provider.send_raw_transaction(tx).await?.tx_hash()),
        }
    }

    pub async fn estimate_eip1559_fees(&self) -> Result<(U256, U256), ProviderError> {
        match self {
            EthProvider::Http(provider) => provider.estimate_eip1559_fees(None).await,
            EthProvider::Ws(provider) => provider.estimate_eip1559_fees(None).await,
        }
    }

    pub async fn subscribe_blocks<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<H256, ProviderError>> + Send + 'a>>, ProviderError>
    {
        match self {
            EthProvider::Http(provider) => {
                let stream = provider.watch_blocks().await?.stream();
                let boxed_stream = Box::pin(stream.map(Ok));
                Ok(boxed_stream)
            }
            EthProvider::Ws(provider) => {
                let stream = provider.subscribe_blocks().await?;
                let boxed_stream = Box::pin(stream.map(|block| {
                    block
                        .hash
                        .ok_or_else(|| ProviderError::CustomError("block has not hash".to_string()))
                }));
                Ok(boxed_stream)
            }
        }
    }
}
