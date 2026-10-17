//! Integración de la delta (`delta::changes`): páginas por revisión completa, cursor que solo
//! avanza con revisiones enteras, y el piso de retención que pide resync en vez de una delta
//! silenciosa.

mod common;

use common::{add_op, TestContext};
use ovaultd_server::delta;

#[tokio::test]
async fn delta_never_splits_batch() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for blob in 1..=3u8 {
        ctx.registrar_blob(blob, 10).await?;
    }

    // Un batch de 3 ops = UNA revisión.
    let outcome = ctx
        .commit(vec![add_op(1, 1, 10), add_op(2, 2, 10), add_op(3, 3, 10)], 0)
        .await?;
    assert_eq!(outcome.rev, 1);

    // limit=1 < 3 ops: la revisión se devuelve ENTERA, jamás cortada a la mitad.
    let pagina = delta::changes(&ctx.pool, ctx.vault_id, 0, 1).await?;
    assert_eq!(pagina.items.len(), 3, "una revisión no se corta jamás");
    assert!(pagina.items.iter().all(|item| item.rev == 1));
    assert_eq!(pagina.next_since, 1);
    assert_eq!(pagina.min_rev, None);

    Ok(())
}

#[tokio::test]
async fn next_since_avanza_solo_con_revision_completa() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for blob in 1..=3u8 {
        ctx.registrar_blob(blob, 10).await?;
    }

    ctx.commit(vec![add_op(1, 1, 10), add_op(2, 2, 10)], 0).await?;
    ctx.commit(vec![add_op(3, 3, 10)], 1).await?;

    // Primera página: cabe la rev 1 entera (2 items), el cursor queda en 1.
    let primera = delta::changes(&ctx.pool, ctx.vault_id, 0, 2).await?;
    assert_eq!(primera.items.len(), 2);
    assert!(primera.items.iter().all(|item| item.rev == 1));
    assert_eq!(primera.next_since, 1);

    // Segunda página: la rev 2, cursor en 2.
    let segunda = delta::changes(&ctx.pool, ctx.vault_id, 1, 2).await?;
    assert_eq!(segunda.items.len(), 1);
    assert_eq!(segunda.items[0].rev, 2);
    assert_eq!(segunda.next_since, 2);

    // Sin cambios: página vacía y el mismo cursor.
    let vacia = delta::changes(&ctx.pool, ctx.vault_id, 2, 2).await?;
    assert!(vacia.items.is_empty());
    assert_eq!(vacia.next_since, 2);
    assert_eq!(vacia.min_rev, None);

    Ok(())
}

#[tokio::test]
async fn since_fuera_de_ventana_pide_resync() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for blob in 1..=2u8 {
        ctx.registrar_blob(blob, 10).await?;
    }

    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;
    ctx.commit(vec![add_op(2, 2, 10)], 1).await?;

    // La retención ya no cubre la rev 1: el cliente quedó fuera de ventana.
    sqlx::query("UPDATE vaults SET min_rev = 2 WHERE id = ?")
        .bind(ctx.vault_id)
        .execute(&ctx.pool)
        .await?;

    let fuera = delta::changes(&ctx.pool, ctx.vault_id, 0, 10).await?;
    assert!(
        fuera.items.is_empty(),
        "fuera de ventana NO se devuelve un pedazo de historia"
    );
    assert_eq!(fuera.next_since, 0);
    assert_eq!(fuera.min_rev, Some(2), "el resync se señaliza, no se esconde");

    // Un since POR ENCIMA del piso ya está dentro de ventana: no hay historia nueva que dar,
    // pero tampoco se pide resync (el piso solo muerde a los cursores que quedaron abajo).
    let adentro = delta::changes(&ctx.pool, ctx.vault_id, 2, 10).await?;
    assert!(adentro.items.is_empty());
    assert_eq!(adentro.next_since, 2);
    assert_eq!(adentro.min_rev, None);

    Ok(())
}
