#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
oracle-score 0.7.0 → 0.8.0: три накопившихся долга одной миграцией.

Запускать из ~/oracle-score:
    python3 patch-oracle-score-0.8.py
    cargo test
    docker run --rm -v "$(pwd)":/code \
      --mount type=volume,source=oracle_score_cache,target=/target \
      --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
      cosmwasm/optimizer:0.17.0

ДОЛГ 1 — `tax_probe` ломает чтение действий.

Поля `variable_amount` и `attestor_may_record` добавлялись в ActionParams
позже, а запись `tax_probe` лежит на мейннете в СТАРОМ формате, без них.
`cw_serde` не прощает отсутствующих полей, поэтому любое чтение этой записи
падает — а вместе с ним и всякий обход ACTIONS целиком.

`#[serde(default)]` на оба поля закрывает это навсегда: старые записи
читаются, недостающие булевы становятся `false` — то есть самым безопасным
значением. Заодно снимается необходимость мигрировать при добавлении
следующего необязательного поля.

ДОЛГ 2 — действие нечем удалить.

`SetAction` умеет только записывать. Обезвредить `tax_probe` можно было
единственным способом: перезаписать его пустышкой с нулевым весом и ценой.
Добавлен `RemoveAction { action }` — удаляет запись из ACTIONS.

Порядок применения после миграции важен: сначала `#[serde(default)]` даёт
прочитать сломанную запись, и только потом `RemoveAction` её убирает.
Наоборот не выйдет — удаление тоже читает.

ДОЛГ 3 — версия. 0.7.0 → 0.8.0.

О «снятии recorder'а» из старого списка: в исходниках контракта нет ни
поля, ни роли с таким именем. Либо это про другой репозиторий, либо уже
сделано. Здесь не трогается.
"""

import sys, io, os, shutil, datetime

files = {
    'state':    'src/state.rs',
    'msg':      'src/msg.rs',
    'contract': 'src/contract.rs',
    'cargo':    'Cargo.toml',
}

for f in files.values():
    if not os.path.exists(f):
        sys.exit('не найден ' + f + ' — запускай из корня ~/oracle-score')

src = {k: io.open(v, encoding='utf-8').read() for k, v in files.items()}

if 'RemoveAction' in src['msg']:
    sys.exit('правка уже наложена — файлы не тронуты')

# ── 1. serde(default) на поздние поля ───────────────────────────────────────
old = """    pub variable_amount: bool,"""
new = """    ///
    /// `serde(default)` обязателен: запись `tax_probe` на мейннете сохранена
    /// ДО появления этого поля, и без значения по умолчанию её чтение падает,
    /// а с ним и всякий обход ACTIONS целиком. `false` — безопасное значение.
    #[serde(default)]
    pub variable_amount: bool,"""
if src['state'].count(old) != 1:
    sys.exit('якорь variable_amount не найден — файлы НЕ изменены')
src['state'] = src['state'].replace(old, new, 1)

old = """    pub attestor_may_record: bool,
}"""
new = """    ///
    /// `serde(default)` по той же причине, что и у `variable_amount`: старые
    /// записи в хранилище этого поля не содержат. `false` означает «аттестор
    /// не может записывать платные действия» — строгая сторона.
    #[serde(default)]
    pub attestor_may_record: bool,
}"""
if src['state'].count(old) != 1:
    sys.exit('якорь attestor_may_record не найден — файлы НЕ изменены')
src['state'] = src['state'].replace(old, new, 1)

# ── 2. RemoveAction в сообщениях ────────────────────────────────────────────
old = """    SetAction { item: ActionItem },"""
new = """    SetAction { item: ActionItem },
    /// Удалить действие из конфигурации.
    ///
    /// Раньше удалять было нечем: `SetAction` умеет только записывать, и
    /// обезвредить ненужное действие можно было единственным способом —
    /// перезаписать пустышкой с нулевым весом и ценой. Так на мейннете и
    /// остался висеть `tax_probe`.
    RemoveAction { action: String },"""
if src['msg'].count(old) != 1:
    sys.exit('якорь SetAction в msg.rs не найден — файлы НЕ изменены')
src['msg'] = src['msg'].replace(old, new, 1)

# ── 3. Диспетчер и обработчик ───────────────────────────────────────────────
old = """        ExecuteMsg::SetAction { item } => exec_set_action(deps, info, item),"""
new = """        ExecuteMsg::SetAction { item } => exec_set_action(deps, info, item),
        ExecuteMsg::RemoveAction { action } => exec_remove_action(deps, info, action),"""
if src['contract'].count(old) != 1:
    sys.exit('якорь диспетчера не найден — файлы НЕ изменены')
src['contract'] = src['contract'].replace(old, new, 1)

old = """#[allow(clippy::too_many_arguments)]
fn exec_update_config("""
new = """fn exec_remove_action(
    deps: DepsMut,
    info: MessageInfo,
    action: String,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    // Проверяем существование до удаления: молчаливый успех на опечатке в
    // ключе оставил бы админа в уверенности, что действие убрано.
    if !ACTIONS.has(deps.storage, action.as_str()) {
        return Err(ContractError::UnknownAction { action });
    }
    ACTIONS.remove(deps.storage, action.as_str());
    Ok(Response::new()
        .add_attribute("action", "remove_action")
        .add_attribute("key", action))
}

#[allow(clippy::too_many_arguments)]
fn exec_update_config("""
if src['contract'].count(old) != 1:
    sys.exit('якорь exec_update_config не найден — файлы НЕ изменены')
src['contract'] = src['contract'].replace(old, new, 1)

# ── 4. Версия ───────────────────────────────────────────────────────────────
old = 'version = "0.7.0"'
new = 'version = "0.8.0"'
if src['cargo'].count(old) != 1:
    sys.exit('версия 0.7.0 не найдена — файлы НЕ изменены')
src['cargo'] = src['cargo'].replace(old, new, 1)

stamp = datetime.datetime.now().strftime('%Y%m%d-%H%M%S')
for k, path in files.items():
    shutil.copy(path, path + '.bak-' + stamp)
    io.open(path, 'w', encoding='utf-8').write(src[k])

print('готово: state.rs, msg.rs, contract.rs, Cargo.toml')
print('копии с меткой .bak-' + stamp)
print('')
print('Дальше: cargo test, затем сборка оптимизатором 0.17.0 и migrate.sh на rebel-2.')
print('ПОРЯДОК НА ЦЕПИ: сначала миграция (она даёт прочитать сломанную запись),')
print('только потом RemoveAction на tax_probe — удаление тоже читает.')
