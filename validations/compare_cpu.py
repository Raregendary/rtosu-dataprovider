import psutil
import time
import requests

tosu_procs = []
rtosu_procs = []

for p in psutil.process_iter(['pid', 'name']):
    name = (p.info['name'] or '').lower()
    if 'tosu' in name and 'rtosu' not in name:
        tosu_procs.append(p)
    elif 'rtosu' in name:
        rtosu_procs.append(p)

for p in tosu_procs + rtosu_procs:
    try:
        p.cpu_percent(interval=None)
    except Exception:
        pass

print("Measuring over 10 seconds...")
time.sleep(10.0)

num_cores = psutil.cpu_count() or 1
tosu_cpu = sum(p.cpu_percent(interval=None) for p in tosu_procs)
rtosu_cpu = sum(p.cpu_percent(interval=None) for p in rtosu_procs)

print(f"tosu CPU:  {tosu_cpu:.2f}% (normalized: {tosu_cpu/num_cores:.2f}%)")
print(f"rtosu CPU: {rtosu_cpu:.2f}% (normalized: {rtosu_cpu/num_cores:.2f}%)")
print(f"Efficiency: rtosu uses {rtosu_cpu/tosu_cpu*100:.1f}% of tosu's CPU (~{tosu_cpu/rtosu_cpu:.1f}x lower)")
