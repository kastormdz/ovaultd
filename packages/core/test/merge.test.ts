import { describe, expect, it } from 'vitest';

import { esMergeablePorTipo, mergeDeTexto } from '../src/merge.js';

describe('merge de texto a 3 vías', () => {
  it('ediciones disjuntas en el MISMO párrafo se fusionan', () => {
    // El caso del plan (`same_paragraph_disjoint_edits_merge`): uno cambia el principio y el
    // otro el final del mismo párrafo. Ningún tramo se pisa, así que se fusiona.
    const base = 'El gato duerme en el sofa.';
    const ours = 'El gato duerme en el sofa rojo.';
    const theirs = 'El gato grande duerme en el sofa.';

    const r = mergeDeTexto(base, ours, theirs);
    expect(r.tipo).toBe('merge');
    if (r.tipo === 'merge') {
      expect(r.texto).toBe('El gato grande duerme en el sofa rojo.');
    }
  });

  it('ediciones en párrafos distintos se fusionan', () => {
    const base = '# Titulo\n\nPrimer parrafo.\n\nSegundo parrafo.\n';
    const ours = '# Titulo\n\nPrimer parrafo editado.\n\nSegundo parrafo.\n';
    const theirs = '# Titulo\n\nPrimer parrafo.\n\nSegundo parrafo tambien editado.\n';

    const r = mergeDeTexto(base, ours, theirs);
    expect(r.tipo).toBe('merge');
    if (r.tipo === 'merge') {
      expect(r.texto).toContain('Primer parrafo editado.');
      expect(r.texto).toContain('Segundo parrafo tambien editado.');
    }
  });

  it('la MISMA linea editada distinto es conflicto', () => {
    const base = 'color: azul\n';
    const ours = 'color: rojo\n';
    const theirs = 'color: verde\n';

    expect(mergeDeTexto(base, ours, theirs).tipo).toBe('conflicto');
  });

  it('inserciones concurrentes en el MISMO punto son conflicto', () => {
    // El plan lo pide explícito: no se inventa un orden para dos textos nuevos en el mismo lugar.
    const base = 'linea uno\nlinea dos\n';
    const ours = 'linea uno\nAGREGADO POR A\nlinea dos\n';
    const theirs = 'linea uno\nAGREGADO POR B\nlinea dos\n';

    expect(mergeDeTexto(base, ours, theirs).tipo).toBe('conflicto');
  });

  it('si uno no cambio, gana el otro (sin fuzzy)', () => {
    const base = 'hola\n';
    const ours = 'hola mundo\n';

    expect(mergeDeTexto(base, ours, base)).toEqual({ tipo: 'merge', texto: 'hola mundo\n' });
    expect(mergeDeTexto(base, base, ours)).toEqual({ tipo: 'merge', texto: 'hola mundo\n' });
  });

  it('si los dos cambiaron igual, converge sin conflicto', () => {
    const base = '# Nota\n';
    const igual = '# Nota editada\n';

    expect(mergeDeTexto(base, igual, igual)).toEqual({ tipo: 'merge', texto: igual });
  });

  it('un reemplazo total y un agregado al final son adyacentes: se fusionan', () => {
    // No se pisan (el cambio de `ours` termina donde empieza el de `theirs`) y ninguno pierde
    // datos: es exactamente el caso que el plan define como fusionable.
    const base = 'contenido original\n';
    const ours = 'otra cosa completamente distinta\n';
    const theirs = 'contenido original\ny una linea nueva\n';

    const r = mergeDeTexto(base, ours, theirs);
    expect(r.tipo).toBe('merge');
    if (r.tipo === 'merge') {
      expect(r.texto).toBe('otra cosa completamente distinta\ny una linea nueva\n');
    }
  });

  it('cambios que se pisan PARCIALMENTE son conflicto', () => {
    // Ninguno cubre todo el texto, pero sus rangos se superponen: decide el usuario.
    const base = 'linea uno\nlinea dos\nlinea tres\n';
    const ours = 'linea uno EDITADA\nlinea dos\nlinea tres\n';
    const theirs = 'linea uno\nlinea dos EDITADA\nlinea tres\n';

    // Ediciones en lineas distintas: fusiona.
    expect(mergeDeTexto(base, ours, theirs).tipo).toBe('merge');
    if (mergeDeTexto(base, ours, theirs).tipo === 'merge') {
      const r = mergeDeTexto(base, ours, theirs);
      if (r.tipo === 'merge') {
        expect(r.texto).toContain('linea uno EDITADA');
        expect(r.texto).toContain('linea dos EDITADA');
      }
    }

    // Ahora las dos tocan la MISMA linea: conflicto.
    const otroOurs = 'linea uno de A\nlinea dos\nlinea tres\n';
    const otroTheirs = 'linea uno de B\nlinea dos\nlinea tres\n';
    expect(mergeDeTexto(base, otroOurs, otroTheirs).tipo).toBe('conflicto');
  });

  it('texto vacio en la base: dos creaciones distintas son conflicto', () => {
    expect(mergeDeTexto('', 'uno\n', 'dos\n').tipo).toBe('conflicto');
    expect(mergeDeTexto('', '', 'dos\n')).toEqual({ tipo: 'merge', texto: 'dos\n' });
  });

  it('el merge no pierde ni duplica contenido', () => {
    const base = 'a\nb\nc\nd\ne\n';
    const ours = 'a\nb MOD\nc\nd\ne\n';
    const theirs = 'a\nb\nc\nd\ne\nf\n';

    const r = mergeDeTexto(base, ours, theirs);
    expect(r.tipo).toBe('merge');
    if (r.tipo === 'merge') {
      expect(r.texto).toBe('a\nb MOD\nc\nd\ne\nf\n');
    }
  });
});

describe('qué tipos admiten merge', () => {
  it('markdown y texto si; canvas y base no', () => {
    expect(esMergeablePorTipo('Notas/uno.md')).toBe(true);
    expect(esMergeablePorTipo('config.json')).toBe(true);
    expect(esMergeablePorTipo('tablero.canvas')).toBe(false);
    expect(esMergeablePorTipo('datos.base')).toBe(false);
    expect(esMergeablePorTipo('TABLERO.CANVAS')).toBe(false);
    expect(esMergeablePorTipo('foto.png')).toBe(false);
    expect(esMergeablePorTipo('sin-extension')).toBe(false);
  });
});
