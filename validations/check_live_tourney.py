import urllib.request
import json
import time

def fetch(url):
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req, timeout=3) as r:
            return json.loads(r.read())
    except Exception as e:
        return {'error': str(e)}

def main():
    tosu = fetch('http://127.0.0.1:24050/json/v2')
    rtosu = fetch('http://127.0.0.1:24051/json/v2')

    print("TOSU state:", tosu.get('state', {}).get('name'), "clients:", len(tosu.get('tourney', {}).get('clients', [])))
    print("RTOSU state:", rtosu.get('state', {}).get('name'), "clients:", len(rtosu.get('tourney', {}).get('clients', [])))

    print("\nTOSU clients:")
    for c in tosu.get('tourney', {}).get('clients', []):
        u = c.get('user', {}) or {}
        p = c.get('play', {}) or {}
        combo = p.get('combo', {}) or {}
        mods = p.get('mods', {}) or {}
        print(f"  ipc={c.get('ipcId')} team={c.get('team')} user={u.get('name')} score={p.get('score')} combo={combo.get('current')} acc={p.get('accuracy')} mods={mods.get('name')}")

    print("\nRTOSU clients:")
    for c in rtosu.get('tourney', {}).get('clients', []):
        u = c.get('user', {}) or {}
        p = c.get('play', {}) or {}
        combo = p.get('combo', {}) or {}
        mods = p.get('mods', {}) or {}
        print(f"  ipc={c.get('ipcId')} team={c.get('team')} user={u.get('name')} score={p.get('score')} combo={combo.get('current')} acc={p.get('accuracy')} mods={mods.get('name')}")

    t_man = tosu.get('tourney', {}).get('manager', {}) or {}
    r_man = rtosu.get('tourney', {}).get('manager', {}) or {}
    print(f"\nTOSU manager: bool={t_man.get('bool')} stars={t_man.get('stars')} teamName={t_man.get('teamName')} ipcCount={t_man.get('ipcClients')}")
    print(f"RTOSU manager: bool={r_man.get('bool')} stars={r_man.get('stars')} teamName={r_man.get('teamName')} ipcCount={r_man.get('ipcClients')}")

    # Root beatmap
    t_bm = tosu.get('beatmap', {}) or {}
    r_bm = rtosu.get('beatmap', {}) or {}
    print(f"\nBeatmap: tosu='{t_bm.get('title')} [{t_bm.get('version')}]' live={t_bm.get('time', {}).get('live')} isKiai={t_bm.get('isKiai')} isBreak={t_bm.get('isBreak')}")
    print(f"Beatmap: rtosu='{r_bm.get('title')} [{r_bm.get('version')}]' live={r_bm.get('time', {}).get('live')} isKiai={r_bm.get('isKiai')} isBreak={r_bm.get('isBreak')}")

if __name__ == '__main__':
    main()
