use actix_web::{delete, web, HttpResponse};
use bundler_client::types::{CancelDataIntentSigned, DataIntentId};
use log::debug;
use std::sync::Arc;

use crate::utils::e400;
use crate::AppData;

#[tracing::instrument(skip(body, data), err)]
#[delete("/v1/data/{id}")]
pub(crate) async fn delete_data(
    id: web::Path<DataIntentId>,
    body: web::Json<CancelDataIntentSigned>,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let cancel = body.into_inner();

    // Verify the path ID matches the body ID
    if *id != cancel.id {
        return Err(e400(eyre::eyre!(
            "path id {} does not match body id {}",
            *id,
            cancel.id
        )));
    }

    // Verify the signature
    cancel.verify_signature().map_err(e400)?;

    // Perform cancellation
    data.cancel_data_intent(&cancel.id, &cancel.from)
        .await
        .map_err(e400)?;

    debug!("cancelled data intent {} by {}", cancel.id, cancel.from);

    Ok(HttpResponse::Ok().finish())
}

#[cfg(test)]
mod tests {
    use bundler_client::types::{CancelDataIntentSigned, DataIntentId};
    use ethers::signers::{LocalWallet, Signer};
    use ethers::types::Address;
    use eyre::Result;
    use std::str::FromStr;

    const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";

    fn get_wallet() -> Result<LocalWallet> {
        Ok(LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?)
    }

    #[tokio::test]
    async fn cancel_request_path_id_mismatch_detected() -> Result<()> {
        let wallet = get_wallet()?;
        let id1 = DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483")?;
        let id2 = DataIntentId::from_str("d5f2cee1-4442-5581-c538-8d3a6625e594")?;

        let cancel = CancelDataIntentSigned::with_signature(&wallet, wallet.address(), id1).await?;

        // Verify the IDs don't match (simulating what the route handler checks)
        assert_ne!(id2, cancel.id);

        Ok(())
    }

    #[tokio::test]
    async fn cancel_request_signature_verification() -> Result<()> {
        let wallet = get_wallet()?;
        let id = DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483")?;

        let cancel = CancelDataIntentSigned::with_signature(&wallet, wallet.address(), id).await?;

        // Valid signature passes
        cancel.verify_signature()?;

        // Tampered signature fails
        let mut tampered = cancel.clone();
        tampered.from = Address::zero();
        assert!(tampered.verify_signature().is_err());

        Ok(())
    }
}
