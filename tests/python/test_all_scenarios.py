"""AXON 全流程全场景测试 — 基于实际 API 验证"""

import sys
import math
import json

passed = 0
failed = 0
skipped = 0
errors = []


def test(name, fn):
    global passed, failed
    try:
        fn()
        print(f'  PASS: {name}')
        passed += 1
    except Exception as e:
        print(f'  FAIL: {name} -> {e}')
        failed += 1
        errors.append((name, str(e)))


def skip(name, reason):
    global skipped
    print(f'  SKIP: {name} ({reason})')
    skipped += 1


import axon_quant


# ===== 场景 2: RL 环境 =====
print('--- 场景 2: RL 环境 ---')

def t_import():
    assert hasattr(axon_quant, 'rl')
    assert hasattr(axon_quant.rl, 'TradingEnv')
    assert hasattr(axon_quant.rl, 'VERSION')
test('2.1 import', t_import)

def t_create_env():
    bars = [{'timestamp': i*60000, 'open': 100.0, 'high': 101.0, 'low': 99.0, 'close': 100.0, 'volume': 1.0} for i in range(10)]
    env = axon_quant.rl.TradingEnv(
        config={'initial_capital': 100_000.0, 'max_steps': 10},
        action_space={'type': 'continuous', 'min': -1.0, 'max': 1.0},
        market_data=bars,
    )
    assert env is not None
test('2.2 create_env', t_create_env)

def t_requires_market_data():
    try:
        axon_quant.rl.TradingEnv(config={'initial_capital': 100_000.0})
        raise AssertionError('Should have raised ValueError')
    except ValueError as e:
        assert 'market_data is required' in str(e)
test('2.2b requires_market_data', t_requires_market_data)

def t_reset():
    bars = [{'timestamp': i*60000, 'open': 100.0, 'high': 101.0, 'low': 99.0, 'close': 100.0+i, 'volume': 1.0} for i in range(10)]
    env = axon_quant.rl.TradingEnv(
        config={'initial_capital': 100_000.0, 'max_steps': 10},
        market_data=bars,
    )
    obs = env.reset()
    # reset() 返回 dict: {features, feature_names, timestamp}
    assert isinstance(obs, dict)
    assert 'features' in obs
    assert 'feature_names' in obs
    assert 'timestamp' in obs
    assert isinstance(obs['features'], list)
    assert len(obs['features']) > 0
    assert not any(math.isnan(x) for x in obs['features'])
test('2.3 reset', t_reset)

def t_step():
    bars = [{'timestamp': i*60000, 'open': 100.0, 'high': 101.0, 'low': 99.0, 'close': 100.0+i, 'volume': 1.0} for i in range(10)]
    env = axon_quant.rl.TradingEnv(
        config={'initial_capital': 100_000.0, 'max_steps': 10},
        market_data=bars,
    )
    env.reset()
    # step() 返回 5 元组: (obs, reward, done, truncated, info)
    result = env.step([0.5])
    assert isinstance(result, tuple)
    assert len(result) == 5
    obs, reward, done, truncated, info = result
    assert isinstance(obs, dict)
    assert 'features' in obs
    assert isinstance(reward, (int, float))
    assert not math.isnan(reward)
    assert isinstance(done, bool)
    assert isinstance(info, dict)
    assert 'portfolio_value' in info
test('2.4 step', t_step)

def t_full_episode():
    bars = [{'timestamp': i*60000, 'open': 100.0, 'high': 101.0, 'low': 99.0, 'close': 100.0+(-1)**i, 'volume': 1.0} for i in range(100)]
    env = axon_quant.rl.TradingEnv(
        config={'initial_capital': 100_000.0, 'max_steps': 50},
        market_data=bars,
    )
    env.reset()
    total_reward = 0.0
    steps = 0
    for _ in range(100):
        obs, reward, done, truncated, info = env.step([0.0])
        total_reward += reward
        steps += 1
        if done or truncated:
            break
    assert steps > 0
    assert not math.isnan(total_reward)
test('2.5 full_episode', t_full_episode)

def t_portfolio_value():
    bars = [{'timestamp': i*60000, 'open': 100.0, 'high': 101.0, 'low': 99.0, 'close': 100.0, 'volume': 1.0} for i in range(10)]
    env = axon_quant.rl.TradingEnv(
        config={'initial_capital': 100_000.0, 'max_steps': 10},
        market_data=bars,
    )
    assert hasattr(env, 'portfolio_value')
    pv = env.portfolio_value
    assert isinstance(pv, (int, float))
    assert pv > 0
test('2.6 portfolio_value', t_portfolio_value)

def t_step_info_fields():
    bars = [{'timestamp': i*60000, 'open': 100.0, 'high': 101.0, 'low': 99.0, 'close': 100.0+i, 'volume': 1.0} for i in range(10)]
    env = axon_quant.rl.TradingEnv(
        config={'initial_capital': 100_000.0, 'max_steps': 10},
        market_data=bars,
    )
    env.reset()
    _, _, _, _, info = env.step([0.5])
    assert 'portfolio_value' in info
    assert 'trades_executed' in info
    assert 'transaction_costs' in info
    assert 'current_step' in info
    assert 'done' in info
test('2.7 step_info_fields', t_step_info_fields)


# ===== 场景 3: HPO =====
print()
print('--- 场景 3: HPO ---')

def t_hpo_import():
    assert hasattr(axon_quant.hpo, 'HPORunner')
    assert hasattr(axon_quant.hpo, 'py_compute_pareto_front')
    assert hasattr(axon_quant.hpo, 'py_compute_hypervolume')
    assert hasattr(axon_quant.hpo, 'py_validate_search_space')
test('3.1 import', t_hpo_import)

def t_pareto_front():
    trials = [{'trial_id': i, 'values': [float(i), float(10-i)]} for i in range(11)]
    front = axon_quant.hpo.py_compute_pareto_front(trials, ['maximize', 'maximize'])
    assert isinstance(front, list)
    assert len(front) > 0
    assert len(front) <= len(trials)
test('3.2 pareto_front', t_pareto_front)

def t_pareto_single_obj():
    trials = [{'trial_id': i, 'values': [float(i)]} for i in range(5)]
    front = axon_quant.hpo.py_compute_pareto_front(trials, ['maximize'])
    assert len(front) == 1
test('3.3 pareto_single_obj', t_pareto_single_obj)

def t_hypervolume():
    # 注意：此项目的 hypervolume 约定是 reference = 右上角（最差值）
    trials = [{'trial_id': i, 'values': v} for i, v in enumerate([[1.0, 1.0], [2.0, 0.5], [0.5, 2.0]])]
    hv = axon_quant.hpo.py_compute_hypervolume(trials, ['maximize', 'maximize'], [3.0, 3.0])
    assert isinstance(hv, (int, float))
    assert hv > 0
test('3.4 hypervolume', t_hypervolume)


# ===== 场景 4: WalkForward =====
print()
print('--- 场景 4: WalkForward ---')

def t_wf_import():
    assert hasattr(axon_quant.walk_forward, 'py_detect_leakage')
    assert hasattr(axon_quant.walk_forward, 'py_embargo_indices')
    assert hasattr(axon_quant.walk_forward, 'py_deflated_sharpe')
    assert hasattr(axon_quant.walk_forward, 'py_aggregate_folds')
    assert hasattr(axon_quant.walk_forward, 'py_purge_overlapping_labels')
test('4.1 import', t_wf_import)

def t_detect_leakage_no():
    has_leak, pairs = axon_quant.walk_forward.py_detect_leakage(list(range(80)), list(range(80, 100)), 0)
    assert has_leak is False
    assert len(pairs) == 0
test('4.2 detect_leakage_no', t_detect_leakage_no)

def t_detect_leakage_yes():
    has_leak, pairs = axon_quant.walk_forward.py_detect_leakage(list(range(90)), list(range(80, 100)), 0)
    assert has_leak is True
    assert len(pairs) > 0
test('4.3 detect_leakage_yes', t_detect_leakage_yes)

def t_detect_leakage_with_lag():
    has_leak, pairs = axon_quant.walk_forward.py_detect_leakage(list(range(80)), list(range(80, 100)), 1)
    assert has_leak is True
    assert len(pairs) > 0
test('4.4 detect_leakage_with_lag', t_detect_leakage_with_lag)

def t_deflated_sharpe():
    dsr = axon_quant.walk_forward.py_deflated_sharpe(2.0, 100, 0.5)
    assert isinstance(dsr, (int, float))
    assert not math.isnan(dsr)
    assert dsr <= 2.0
test('4.5 deflated_sharpe', t_deflated_sharpe)

def t_embargo():
    embargo = axon_quant.walk_forward.py_embargo_indices(list(range(80, 100)), 0.1, 100)
    assert isinstance(embargo, list)
test('4.6 embargo', t_embargo)


# ===== 场景 5: Tracker =====
print()
print('--- 场景 5: Tracker ---')

def t_tracker_import():
    assert hasattr(axon_quant.tracker, 'MemoryTracker')
    assert hasattr(axon_quant.tracker, 'LocalTracker')
test('5.1 import', t_tracker_import)

def t_tracker_lifecycle():
    mem = axon_quant.tracker.MemoryTracker()
    mem.log_param('lr', 0.001)
    mem.log_param('batch_size', 32)
    mem.log_metric('loss', 0.5, step=1)
    mem.log_metric('loss', 0.3, step=2)
    mem.set_tag('model', 'ppo')
    metrics = mem.get_metrics()
    assert isinstance(metrics, dict)
    mem.finish('completed')
test('5.2 tracker_lifecycle', t_tracker_lifecycle)


# ===== 场景 6: Distributed =====
print()
print('--- 场景 6: Distributed ---')

def t_distributed_import():
    assert hasattr(axon_quant.distributed, 'DistributedRunner')
    assert hasattr(axon_quant.distributed, 'py_serialize_metrics')
    assert hasattr(axon_quant.distributed, 'py_save_checkpoint')
    assert hasattr(axon_quant.distributed, 'py_load_checkpoint')
test('6.1 import', t_distributed_import)

def t_serialize_metrics():
    s = axon_quant.distributed.py_serialize_metrics(100, 0.5, 0.01, 0.02, 0.1, 1000.0)
    assert isinstance(s, str)
    assert len(s) > 0
    data = json.loads(s)
    assert isinstance(data, dict)
test('6.2 serialize_metrics', t_serialize_metrics)


# ===== 场景 7: Registry =====
print()
print('--- 场景 7: Registry ---')

def t_registry_import():
    assert hasattr(axon_quant.registry, 'ModelRegistry')
    assert hasattr(axon_quant.registry, 'LocalStorage')
test('7.1 import', t_registry_import)

def t_registry_classes():
    assert callable(axon_quant.registry.ModelRegistry)
    assert callable(axon_quant.registry.LocalStorage)
test('7.2 classes', t_registry_classes)


# ===== 汇总 =====
print()
print('=' * 50)
print(f'结果: {passed} passed, {failed} failed, {skipped} skipped')
if errors:
    print()
    print('失败项:')
    for name, err in errors:
        print(f'  {name}: {err}')
print('=' * 50)

sys.exit(1 if failed > 0 else 0)
