//! Fechas de SQLite (UTC) al huso del despliegue.
//!
//! `datetime('now')` es SIEMPRE UTC, y el panel imprimía ese string crudo como si fuera
//! hora local: en Argentina se veía tres horas adelantado (00:58 cuando eran las 21:58).
//! El server no trae `chrono` y la imagen no garantiza `tzdata`, así que la conversión es
//! aritmética civil pura con un offset configurable (`OVAULTD_TZ_OFFSET_MINUTES`).

/// Suma `offset_minutos` a un timestamp `YYYY-MM-DD HH:MM:SS` de SQLite.
///
/// Si el string no tiene ese formato se devuelve tal cual: una fecha rara no puede
/// tumbar el panel.
pub fn hora_local(ts: &str, offset_minutos: i32) -> String {
    convertir(ts, i64::from(offset_minutos)).unwrap_or_else(|| ts.to_owned())
}

/// Etiqueta del huso para los encabezados: `UTC`, `UTC-03:00`, `UTC+05:30`.
pub fn etiqueta_zona(offset_minutos: i32) -> String {
    if offset_minutos == 0 {
        return "UTC".to_owned();
    }
    let signo = if offset_minutos < 0 { '-' } else { '+' };
    let absoluto = offset_minutos.unsigned_abs();
    format!("UTC{signo}{:02}:{:02}", absoluto / 60, absoluto % 60)
}

fn convertir(ts: &str, offset_minutos: i64) -> Option<String> {
    let (fecha, hora) = ts.split_once(' ')?;
    let mut partes_fecha = fecha.split('-');
    let anio: i64 = partes_fecha.next()?.parse().ok()?;
    let mes: i64 = partes_fecha.next()?.parse().ok()?;
    let dia: i64 = partes_fecha.next()?.parse().ok()?;

    let mut partes_hora = hora.split(':');
    let hh: i64 = partes_hora.next()?.parse().ok()?;
    let mm: i64 = partes_hora.next()?.parse().ok()?;
    let ss: i64 = partes_hora.next()?.parse().ok()?;

    if !(1..=12).contains(&mes) || !(1..=31).contains(&dia) || !(0..=23).contains(&hh)
        || !(0..=59).contains(&mm) || !(0..=59).contains(&ss)
    {
        return None;
    }

    let total =
        dias_desde_epoca(anio, mes, dia) * 86_400 + hh * 3_600 + mm * 60 + ss + offset_minutos * 60;
    let dias = total.div_euclid(86_400);
    let resto = total.rem_euclid(86_400);
    let (anio_local, mes_local, dia_local) = civil_desde_dias(dias);

    Some(format!(
        "{anio_local:04}-{mes_local:02}-{dia_local:02} {:02}:{:02}:{:02}",
        resto / 3_600,
        (resto % 3_600) / 60,
        resto % 60
    ))
}

/// Días desde 1970-01-01 de una fecha civil (algoritmo de Howard Hinnant).
fn dias_desde_epoca(anio: i64, mes: i64, dia: i64) -> i64 {
    let anio = if mes <= 2 { anio - 1 } else { anio };
    let era = if anio >= 0 { anio } else { anio - 399 } / 400;
    let anio_de_era = anio - era * 400;
    let mes_pos = (mes + 9) % 12;
    let dia_del_anio = (153 * mes_pos + 2) / 5 + dia - 1;
    let dia_de_era = anio_de_era * 365 + anio_de_era / 4 - anio_de_era / 100 + dia_del_anio;
    era * 146_097 + dia_de_era - 719_468
}

/// Inversa de `dias_desde_epoca`.
fn civil_desde_dias(dias: i64) -> (i64, i64, i64) {
    let dias = dias + 719_468;
    let era = if dias >= 0 { dias } else { dias - 146_096 } / 146_097;
    let dia_de_era = dias - era * 146_097;
    let anio_de_era =
        (dia_de_era - dia_de_era / 1_460 + dia_de_era / 36_524 - dia_de_era / 146_096) / 365;
    let anio = anio_de_era + era * 400;
    let dia_del_anio = dia_de_era - (365 * anio_de_era + anio_de_era / 4 - anio_de_era / 100);
    let mes_pos = (5 * dia_del_anio + 2) / 153;
    let dia = dia_del_anio - (153 * mes_pos + 2) / 5 + 1;
    let mes = if mes_pos < 10 { mes_pos + 3 } else { mes_pos - 9 };
    (if mes <= 2 { anio + 1 } else { anio }, mes, dia)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resta_tres_horas_y_cruza_la_medianoche() {
        assert_eq!(hora_local("2026-10-24 02:44:36", -180), "2026-10-23 23:44:36");
        assert_eq!(hora_local("2026-10-24 00:10:00", -180), "2026-10-23 21:10:00");
    }

    #[test]
    fn suma_horas_y_cruza_al_dia_siguiente() {
        assert_eq!(hora_local("2026-12-31 23:30:00", 60), "2027-01-01 00:30:00");
    }

    #[test]
    fn soporta_medias_horas() {
        assert_eq!(hora_local("2026-10-24 00:00:00", 330), "2026-10-24 05:30:00");
        assert_eq!(hora_local("2026-10-24 00:00:00", -210), "2026-10-23 20:30:00");
    }

    #[test]
    fn ano_bisiesto() {
        assert_eq!(hora_local("2028-02-29 01:00:00", -60), "2028-02-29 00:00:00");
        assert_eq!(hora_local("2028-03-01 00:30:00", -60), "2028-02-29 23:30:00");
    }

    #[test]
    fn utc_no_cambia() {
        assert_eq!(hora_local("2026-10-24 02:44:36", 0), "2026-10-24 02:44:36");
    }

    #[test]
    fn un_string_raro_se_devuelve_intacto() {
        assert_eq!(hora_local("basura", -180), "basura");
        assert_eq!(hora_local("", -180), "");
        assert_eq!(hora_local("2026-13-40 99:99:99", -180), "2026-13-40 99:99:99");
    }

    #[test]
    fn etiqueta_de_zona() {
        assert_eq!(etiqueta_zona(0), "UTC");
        assert_eq!(etiqueta_zona(-180), "UTC-03:00");
        assert_eq!(etiqueta_zona(330), "UTC+05:30");
        assert_eq!(etiqueta_zona(45), "UTC+00:45");
    }
}
