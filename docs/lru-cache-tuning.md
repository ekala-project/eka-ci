# LRU Cache Operational Runbook

**Version:** 1.0
**Date:** 2026-04-07
**Status:** Production Ready

## Table of Contents

1. [Quick Reference](#quick-reference)
2. [Monitoring](#monitoring)
3. [Capacity Tuning](#capacity-tuning)
4. [Troubleshooting](#troubleshooting)
5. [Alerts](#alerts)
6. [Performance Optimization](#performance-optimization)

---

## Quick Reference

### Configuration

**Environment Variable:**
```bash
export EKA_CI_GRAPH_LRU_CAPACITY=100000
```

**Config File** (`~/.config/ekaci/ekaci.toml`):
```toml
graph_lru_capacity = 100000
```

**Default:** 100,000 nodes

### Key Metrics

| Metric | Description | Healthy Range |
|--------|-------------|---------------|
| `eka_ci_graph_cache_utilization` | Cache fullness (0.0-1.0) | 0.5 - 0.8 |
| `eka_ci_graph_cache_reloads_total` | Cache misses (counter) | < 100/day |
| `eka_ci_graph_pinned_nodes_total` | Protected nodes | 50 - 500 |
| `eka_ci_graph_nodes_total` | Total nodes | < capacity |

### Log Messages

**Normal Operation:**
```
INFO Cache status: 45000/100000 nodes (45.0% utilized), 123 pinned
```

**Warning (80% utilization):**
```
WARN Cache utilization elevated (82.3%): Monitor for potential capacity issues
```

**Critical (90% utilization):**
```
WARN Cache utilization HIGH (93.1%): Consider increasing EKA_CI_GRAPH_LRU_CAPACITY (current: 100000)
```

---

## Monitoring

### Grafana Dashboard

#### Panel 1: Cache Utilization (Gauge)
```promql
eka_ci_graph_cache_utilization * 100
```
- **Unit:** Percent
- **Thresholds:**
  - Green: < 70%
  - Yellow: 70-85%
  - Red: > 85%

#### Panel 2: Cache Size (Graph)
```promql
sum(eka_ci_graph_nodes_total)
```
- **Unit:** Nodes
- **Show:** Current, Max capacity

#### Panel 3: Cache Reload Rate (Graph)
```promql
rate(eka_ci_graph_cache_reloads_total[5m]) * 60
```
- **Unit:** Reloads/min
- **Alert:** > 10/min for 15 minutes

#### Panel 4: Reload Latency (Graph)
```promql
histogram_quantile(0.50, rate(eka_ci_graph_cache_reload_duration_seconds_bucket[5m]))
histogram_quantile(0.90, rate(eka_ci_graph_cache_reload_duration_seconds_bucket[5m]))
histogram_quantile(0.99, rate(eka_ci_graph_cache_reload_duration_seconds_bucket[5m]))
```
- **Unit:** Seconds
- **Labels:** p50, p90, p99

#### Panel 5: Pinned Nodes (Stat)
```promql
eka_ci_graph_pinned_nodes_total
```
- **Unit:** Nodes
- **Description:** Active builds

#### Panel 6: Eviction Candidates by Tier (Stacked Graph)
```promql
eka_ci_graph_eviction_candidates_total{tier="tier1_transitive_failure"}
eka_ci_graph_eviction_candidates_total{tier="tier2_completed_failure"}
eka_ci_graph_eviction_candidates_total{tier="tier3_completed_success"}
```

### Key Performance Indicators (KPIs)

**Healthy System:**
- Utilization: 50-70%
- Reload rate: < 5/min
- Reload latency (p99): < 50ms
- Pinned nodes: 50-200

**Concerning:**
- Utilization: > 80%
- Reload rate: > 10/min
- Reload latency (p99): > 100ms
- Pinned nodes: > 1000

**Critical:**
- Utilization: > 90%
- Reload rate: > 50/min
- Reload latency (p99): > 500ms
- Cache thrashing

---

## Capacity Tuning

### Determining Optimal Capacity

**Formula:**
```
Optimal Capacity = (Peak Node Count × 1.5) + Buffer
```

**Example:**
- Peak node count: 60,000
- Optimal capacity: 60,000 × 1.5 = 90,000
- Add buffer: 90,000 + 10,000 = **100,000**

### Capacity Sizing Guide

| Workload | Node Count | Recommended Capacity | Memory Usage |
|----------|------------|---------------------|--------------|
| Small | < 10k | 20,000 | ~22 MB |
| Medium | 10k - 50k | 75,000 | ~83 MB |
| Large | 50k - 100k | 150,000 | ~165 MB |
| Very Large | 100k - 200k | 300,000 | ~330 MB |

### Increasing Capacity

**When to increase:**
- Utilization consistently > 80%
- Reload rate > 10/min
- Warnings in logs every 5 minutes

**How to increase:**

1. **Calculate new capacity:**
   ```
   New Capacity = Current Capacity × 1.5
   ```

2. **Set environment variable:**
   ```bash
   export EKA_CI_GRAPH_LRU_CAPACITY=150000
   ```

3. **Restart service:**
   ```bash
   systemctl restart eka-ci
   ```

4. **Monitor for 1 hour:**
   ```promql
   eka_ci_graph_cache_utilization
   ```

5. **Verify:**
   - Utilization < 70%
   - Reload rate < 5/min
   - No warnings

### Decreasing Capacity

**When to decrease:**
- Utilization consistently < 30%
- Memory usage high (> 200 MB)
- Zero cache reloads for 24+ hours

**How to decrease:**

1. **Calculate new capacity:**
   ```
   New Capacity = Peak Node Count × 1.3
   ```

2. **Set environment variable:**
   ```bash
   export EKA_CI_GRAPH_LRU_CAPACITY=75000
   ```

3. **Restart service:**
   ```bash
   systemctl restart eka-ci
   ```

4. **Monitor closely for 24 hours:**
   - Watch reload rate (should stay < 10/min)
   - Monitor utilization (should be 50-70%)

---

## Troubleshooting

### Problem 1: High Utilization (> 90%)

**Symptoms:**
- Log warnings every 5 minutes
- Potential cache thrashing
- Slow build dispatch

**Diagnosis:**
```promql
# Check utilization
eka_ci_graph_cache_utilization

# Check growth rate
rate(sum(eka_ci_graph_nodes_total)[1h])
```

**Solution:**
1. **Immediate:** Increase capacity by 50%
   ```bash
   export EKA_CI_GRAPH_LRU_CAPACITY=150000
   systemctl restart eka-ci
   ```

2. **Long-term:** Calculate proper capacity based on workload

**Prevention:**
- Set alert for 85% utilization
- Review capacity quarterly

---

### Problem 2: High Reload Rate (> 10/min)

**Symptoms:**
- Frequent cache misses
- Elevated database load
- Slow API responses

**Diagnosis:**
```promql
# Reload rate
rate(eka_ci_graph_cache_reloads_total[5m]) * 60

# Which nodes are being reloaded?
# Check logs for "Cache miss: reloading"
```

**Possible Causes:**

#### Cause 1: Capacity Too Small
- Utilization > 85%
- Solution: Increase capacity

#### Cause 2: Workload Pattern Changed
- Many terminal nodes evicted, then accessed again
- Solution: Increase tier age thresholds

#### Cause 3: Hot Path Not Protected
- `is_buildable()` nodes being evicted
- Solution: Ensure `touch_buildable_check()` is called

---

### Problem 3: High Memory Usage

**Symptoms:**
- Process memory > 500 MB
- OOM risk
- Swap usage

**Diagnosis:**
```promql
# Memory estimate
eka_ci_graph_memory_bytes_estimate

# Utilization
eka_ci_graph_cache_utilization
```

**Solutions:**

#### If utilization < 50%:
- **Cause:** Capacity too large
- **Fix:** Decrease capacity to match peak workload

#### If utilization > 80%:
- **Cause:** Legitimate high usage
- **Fix:** Add more RAM or optimize elsewhere

---

### Problem 4: Zero Reloads Despite Low Utilization

**Symptoms:**
- Utilization < 30%
- Zero cache reloads for days
- High memory usage

**Diagnosis:**
```promql
# Reload count
eka_ci_graph_cache_reloads_total

# Utilization
eka_ci_graph_cache_utilization
```

**Cause:** Capacity oversized

**Solution:**
1. Decrease capacity to improve efficiency
2. Free up memory for other services

---

### Problem 5: Slow Reload Latency (p99 > 100ms)

**Symptoms:**
- High reload latency
- Slow API responses
- Database contention

**Diagnosis:**
```promql
# Reload latency
histogram_quantile(0.99, rate(eka_ci_graph_cache_reload_duration_seconds_bucket[5m]))

# Reload rate
rate(eka_ci_graph_cache_reloads_total[5m])
```

**Possible Causes:**

#### Cause 1: High Reload Rate
- Too many concurrent reloads
- Database overwhelmed
- Solution: Increase capacity to reduce reload frequency

#### Cause 2: Database Slow
- Check database metrics
- Optimize queries
- Add indexes if needed

#### Cause 3: Large Nodes
- Nodes with many dependencies
- Solution: Optimize edge loading (future work)

---

## Alerts

### Prometheus Alert Rules

```yaml
groups:
  - name: lru_cache_alerts
    rules:
      # Critical: High utilization
      - alert: LRUCacheUtilizationHigh
        expr: eka_ci_graph_cache_utilization > 0.90
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: "LRU cache utilization is high ({{ $value | humanizePercentage }})"
          description: "Cache is {{ $value | humanizePercentage }} full. Consider increasing capacity."

      # Warning: Elevated utilization
      - alert: LRUCacheUtilizationElevated
        expr: eka_ci_graph_cache_utilization > 0.80
        for: 1h
        labels:
          severity: info
        annotations:
          summary: "LRU cache utilization is elevated ({{ $value | humanizePercentage }})"
          description: "Cache is {{ $value | humanizePercentage }} full. Monitor for growth."

      # Critical: High reload rate
      - alert: LRUCacheReloadRateHigh
        expr: rate(eka_ci_graph_cache_reloads_total[5m]) * 60 > 10
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: "Cache reload rate is high ({{ $value }} reloads/min)"
          description: "Frequent cache misses detected. Capacity may be too small."

      # Warning: Slow reloads
      - alert: LRUCacheReloadSlow
        expr: histogram_quantile(0.99, rate(eka_ci_graph_cache_reload_duration_seconds_bucket[5m])) > 0.1
        for: 15m
        labels:
          severity: info
        annotations:
          summary: "Cache reloads are slow (p99: {{ $value }}s)"
          description: "Database may be under load or capacity is causing thrashing."

      # Info: Many pinned nodes
      - alert: LRUCacheManyPinnedNodes
        expr: eka_ci_graph_pinned_nodes_total > 1000
        for: 30m
        labels:
          severity: info
        annotations:
          summary: "Many nodes pinned ({{ $value }})"
          description: "High number of active builds. This is normal during large builds."
```

---

## Performance Optimization

### Best Practices

1. **Set Capacity to 1.5× Peak Usage**
   - Provides headroom for growth
   - Minimizes reload rate
   - Optimal utilization: 60-70%

2. **Call `touch_buildable_check()` After `is_buildable()`**
   - Protects hot path nodes
   - Prevents thrashing on active builds
   ```rust
   if graph_handle.is_buildable(&drv_id) {
       graph_handle.touch_buildable_check(&drv_id);
       // ... dispatch build ...
   }
   ```

3. **Monitor Utilization Trends**
   - Review every quarter
   - Adjust capacity as workload changes
   - Plan for growth

4. **Avoid Frequent Restarts**
   - LRU cache is warmed up over time
   - Restarts cause cold cache (100% reload rate initially)
   - Allow 1 hour for warmup

### Capacity Planning

**Formula for Growth:**
```
Future Capacity = Current Peak × Growth Factor × Headroom

Where:
- Growth Factor = Expected growth (1.2 = 20% growth)
- Headroom = Safety margin (1.5 = 50% headroom)
```

**Example:**
- Current peak: 50,000 nodes
- Expected 20% growth: 50,000 × 1.2 = 60,000
- With 50% headroom: 60,000 × 1.5 = **90,000**

---

## Common Scenarios

### Scenario 1: Large Build (200k drvs)

**Expected Behavior:**
- Utilization rises to 80-90%
- Pinned nodes: 500-2000 (active builds)
- Reload rate: 5-10/min (terminal nodes evicted)
- Warnings logged (normal)

**Action:** Monitor, no action needed unless reload rate > 20/min

---

### Scenario 2: Idle System

**Expected Behavior:**
- Utilization: 5-10% (only completed builds)
- Pinned nodes: 0-5
- Reload rate: 0/min
- No warnings

**Action:** Consider decreasing capacity to save memory

---

### Scenario 3: Continuous Integration

**Expected Behavior:**
- Utilization: 40-60% (steady state)
- Pinned nodes: 50-200 (concurrent builds)
- Reload rate: < 5/min
- No warnings

**Action:** Optimal state, no action needed

---

## Maintenance

### Quarterly Review

1. **Check peak utilization (last 90 days):**
   ```promql
   max_over_time(eka_ci_graph_cache_utilization[90d])
   ```

2. **Check reload rate:**
   ```promql
   avg_over_time(rate(eka_ci_graph_cache_reloads_total[1h])[90d:1h]) * 60
   ```

3. **Adjust capacity if needed:**
   - If peak > 80%: Increase by 50%
   - If peak < 40%: Decrease by 25%

### Version Upgrades

**Before upgrade:**
- Note current capacity setting
- Export metrics for comparison

**After upgrade:**
- Verify capacity setting persists
- Compare metrics (should be similar)
- Monitor for 24 hours

---

## Emergency Procedures

### Cache Thrashing (Reload Rate > 50/min)

**Immediate Action:**
1. Double capacity:
   ```bash
   export EKA_CI_GRAPH_LRU_CAPACITY=200000
   systemctl restart eka-ci
   ```

2. Monitor for 15 minutes
3. If still thrashing, double again

**Follow-up:**
- Investigate root cause
- Review workload patterns
- Consider permanent capacity increase

---

### Out of Memory

**Immediate Action:**
1. Restart service (clears cache):
   ```bash
   systemctl restart eka-ci
   ```

2. Reduce capacity by 50%:
   ```bash
   export EKA_CI_GRAPH_LRU_CAPACITY=50000
   systemctl start eka-ci
   ```

3. Monitor memory usage

**Follow-up:**
- Identify memory leak (if any)
- Right-size capacity for available RAM
- Consider adding more RAM

---

## Support

### Logs to Collect

```bash
# Cache status logs (last hour)
journalctl -u eka-ci --since "1 hour ago" | grep "Cache status"

# Warnings (last 24 hours)
journalctl -u eka-ci --since "1 day ago" | grep -E "WARN|ERROR"

# Cache misses (last hour)
journalctl -u eka-ci --since "1 hour ago" | grep "Cache miss"
```

### Metrics to Export

```bash
# Current state
curl http://localhost:8080/metrics | grep eka_ci_graph

# Or via Prometheus query
eka_ci_graph_cache_utilization
eka_ci_graph_cache_reloads_total
eka_ci_graph_nodes_total
```

---

## Summary

**Key Takeaways:**
1. **Monitor utilization** - Keep between 50-80%
2. **Watch reload rate** - Should be < 5/min normally
3. **Tune capacity** - 1.5× peak usage is optimal
4. **Set alerts** - For 85% utilization and high reload rate
5. **Review quarterly** - Adjust as workload changes

**Healthy System Checklist:**
- ✅ Utilization: 50-70%
- ✅ Reload rate: < 5/min
- ✅ No warnings in logs
- ✅ Pinned nodes: 50-500
- ✅ Reload latency (p99): < 50ms

