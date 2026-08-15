#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
oracle-score: тесты на RemoveAction.

Запускать из ~/oracle-score:
    python3 patch-remove-action-tests.py
    cargo test

ЗАЧЕМ. `RemoveAction` — новая ветка, которая трогает хранилище, и её ничто
не проверяет. В наборе уже есть `only_admin_can_prune` и
`only_admin_can_update_config`; удаление действия просится туда же.

Три случая.

1. Админ удаляет — до удаления оплата проходит, после возвращает
   UnknownAction. Проверка идёт через реальную оплату, а не через геттер:
   так видно, что действие перестало существовать ДЛЯ КОНТРАКТА, а не просто
   пропало из выдачи запроса.

2. Посторонний не может — Unauthorized, и действие остаётся рабочим.

3. Опечатка в ключе даёт UnknownAction, а не молчаливый успех. Это главный
   из трёх: операция, которая ничего не сделала, но отчиталась об успехе, —
   ровно то, из-за чего провал выплат разработке три месяца шёл мимо зелёных
   галочек в Actions.
"""

import sys, io, os, shutil, datetime

SRC = 'tests/integration.rs'

if not os.path.exists(SRC):
    sys.exit('не найден ' + SRC + ' — запускай из корня ~/oracle-score')

s = io.open(SRC, encoding='utf-8').read()

if 'remove_action' in s:
    sys.exit('правка уже наложена — файл не тронут')
if 'RemoveAction' not in io.open('src/msg.rs', encoding='utf-8').read():
    sys.exit('в контракте нет RemoveAction — сначала patch-oracle-score-0.8.py')

TESTS = '''
// ── RemoveAction ────────────────────────────────────────────────────────────
// Удаление проверяется реальной оплатой, а не запросом: важно, что действие
// перестало существовать ДЛЯ КОНТРАКТА, а не просто пропало из выдачи.

#[test]
fn admin_can_remove_an_action() {
    let (mut app, c) = setup();

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::SetAction {
            item: ActionItem {
                key: "throwaway".to_string(),
                params: params(10, vec![], 1_000, 0, 0),
            },
        },
        &[],
    )
    .unwrap();

    // Пока действие есть — оплата проходит
    pay(&mut app, &c, USER, "throwaway", "r1", &coins(1_000, DENOM)).unwrap();

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::RemoveAction { action: "throwaway".to_string() },
        &[],
    )
    .unwrap();

    // После удаления контракт о нём не знает
    let err = pay(&mut app, &c, USER, "throwaway", "r2", &coins(1_000, DENOM)).unwrap_err();
    assert!(err.to_string().contains("Unknown action"));
}

#[test]
fn only_admin_can_remove_an_action() {
    let (mut app, c) = setup();

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::SetAction {
            item: ActionItem {
                key: "throwaway".to_string(),
                params: params(10, vec![], 1_000, 0, 0),
            },
        },
        &[],
    )
    .unwrap();

    let err = app
        .execute_contract(
            Addr::unchecked(OTHER),
            c.clone(),
            &ExecuteMsg::RemoveAction { action: "throwaway".to_string() },
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unauthorized"));

    // Действие на месте и работает
    pay(&mut app, &c, USER, "throwaway", "r1", &coins(1_000, DENOM)).unwrap();
}

#[test]
fn removing_a_missing_action_is_an_error_not_a_silent_success() {
    let (mut app, c) = setup();

    let err = app
        .execute_contract(
            Addr::unchecked(ADMIN),
            c.clone(),
            &ExecuteMsg::RemoveAction { action: "never_existed".to_string() },
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unknown action"));
}
'''

s = s.rstrip('\n') + '\n' + TESTS

stamp = datetime.datetime.now().strftime('%Y%m%d-%H%M%S')
shutil.copy(SRC, SRC + '.bak-' + stamp)
io.open(SRC, 'w', encoding='utf-8').write(s)

print('добавлено 3 теста')
print('копия прежнего файла: %s.bak-%s' % (SRC, stamp))
print('дальше: cargo test — ждём 47 пройденных вместо 44')
