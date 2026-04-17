mod optuna_reader;

use clap::Parser;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use log::{error, info};

use reqwest::blocking::Client;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use optuna_reader::OptunaReader;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, env = "HOST", default_value = "0.0.0.0")]
    host: String,

    #[clap(short, long, env = "PORT", default_value_t = 8089)]
    port: u16,

    #[clap(long, env = "PUSH_GATEWAY_URL", default_value = "http://pushgateway:9091")]
    push_gateway_url: String,

    #[clap(long, env = "GODON_API_URL", default_value = "http://godon-api:8080")]
    api_url: String,

    #[clap(long, default_value = "INFO")]
    log_level: String,
}

struct MetricsCache {
    metrics_text: String,
    pushgateway_reachable: f64,
    last_error: String,
}

struct ObserverState {
    push_gateway_url: String,
    cache: Arc<Mutex<MetricsCache>>,
    http_client: Client,
    optuna: OptunaReader,
    api_url: String,
}

impl ObserverState {
    fn new(push_gateway_url: String, api_url: String) -> Self {
        Self {
            push_gateway_url,
            cache: Arc::new(Mutex::new(MetricsCache {
                metrics_text: String::new(),
                pushgateway_reachable: 0.0,
                last_error: String::new(),
            })),
            http_client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            optuna: OptunaReader::from_env(),
            api_url,
        }
    }

    fn fetch_metrics(&self) {
        let url = format!("{}/metrics", self.push_gateway_url);
        match self.http_client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                if let Ok(text) = response.text() {
                    let mut cache = self.cache.lock().unwrap();
                    cache.metrics_text = text;
                    cache.pushgateway_reachable = 1.0;
                    cache.last_error = String::new();
                    info!("Successfully fetched metrics from Push Gateway");
                }
            }
            Ok(response) => {
                let mut cache = self.cache.lock().unwrap();
                cache.pushgateway_reachable = 0.0;
                cache.last_error = format!("HTTP {}", response.status());
                error!("Push Gateway returned: HTTP {}", response.status());
            }
            Err(e) => {
                let mut cache = self.cache.lock().unwrap();
                cache.pushgateway_reachable = 0.0;
                cache.last_error = format!("Connection failed: {}", e);
                error!("Push Gateway connection failed: {}", e);
            }
        }
    }

    fn get_metrics_text(&self) -> String {
        let cache = self.cache.lock().unwrap();
        let mut output = String::new();

        output.push_str("# HELP godon_observer_up Status of the Godon observer\n");
        output.push_str("# TYPE godon_observer_up gauge\n");
        output.push_str("godon_observer_up{status=\"success\"} 1\n\n");

        output.push_str("# HELP godon_observer_pushgateway_reachable Whether Push Gateway is reachable\n");
        output.push_str("# TYPE godon_observer_pushgateway_reachable gauge\n");
        output.push_str(&format!("godon_observer_pushgateway_reachable {}\n\n", cache.pushgateway_reachable));

        if cache.pushgateway_reachable == 1.0 && !cache.metrics_text.is_empty() {
            for line in cache.metrics_text.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    output.push_str(trimmed);
                    output.push('\n');
                }
            }
        }

        output
    }
}

fn json_response(status: StatusCode, body: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn html_response(body: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn parse_query(uri: &hyper::Uri) -> std::collections::HashMap<String, String> {
    uri.query()
        .map(|q| {
            q.split('&')
                .filter_map(|pair| {
                    let mut kv = pair.split('=');
                    Some((kv.next()?.to_string(), kv.next()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn handle_request(req: Request<Body>, state: Arc<ObserverState>) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path().to_string();
    let path_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if path == "/metrics" {
        state.fetch_metrics();
        let metrics_text = state.get_metrics_text();
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .body(Body::from(metrics_text))
            .unwrap());
    }

    if path == "/health" {
        let db_ok = state.optuna.health_check().await;
        let body = if db_ok { "OK" } else { "DEGRADED: db unreachable" };
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body))
            .unwrap());
    }

    if path == "/dashboard" || path == "/dashboard/" {
        return Ok(html_response(DASHBOARD_HTML));
    }

    // /api-proxy/breeders/<uuid> — proxy to godon-api for breeder config
    if path_parts.len() >= 3 && path_parts[0] == "api-proxy" && path_parts[1] == "breeders" {
        let api_path = format!("/breeders/{}", path_parts[2]);
        let url = format!("{}{}", state.api_url, api_path);
        return match state.http_client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                let body = response.text().unwrap_or_default();
                Ok(json_response(StatusCode::OK, &body))
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                Ok(json_response(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY), &body))
            }
            Err(e) => {
                error!("API proxy error: {}", e);
                Ok(json_response(StatusCode::BAD_GATEWAY, &format!("{{\"error\": \"api unreachable: {}\"}}", e)))
            }
        };
    }

    // /api/breeders/<uuid>/summary
    if path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "summary" {
        let breeder_id = path_parts[2].to_string();
        let study_name = format!("{}_study", breeder_id);

        let count = state.optuna.get_trial_count(&breeder_id, &study_name).await.unwrap_or(0);
        let attrs = state.optuna.get_study_user_attrs(&breeder_id, &study_name).await.unwrap_or_default();

        let json = serde_json::json!({
            "breeder_id": breeder_id,
            "study_name": study_name,
            "total_trials": count,
            "study_user_attributes": attrs,
        });
        return Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()));
    }

    // /api/breeders/<uuid>/studies
    if path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "studies" {
        let breeder_id = path_parts[2].to_string();
        return match state.optuna.list_studies(&breeder_id).await {
            Ok(studies) => {
                let json = serde_json::json!({"breeder_id": breeder_id, "studies": studies});
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to list studies: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        };
    }

    // /api/breeders/<uuid>/trials/<study_name>
    if path_parts.len() >= 5 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "trials" {
        let breeder_id = path_parts[2].to_string();
        let study_name = path_parts[4].to_string();
        let query = parse_query(req.uri());
        let offset: i64 = query.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);

        return match state.optuna.get_trials(&breeder_id, &study_name, offset, limit).await {
            Ok(trials) => {
                let json = serde_json::json!({
                    "breeder_id": breeder_id,
                    "study_name": study_name,
                    "offset": offset,
                    "limit": limit,
                    "trials": trials,
                });
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to load trials: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        };
    }

    // /api/breeders/<uuid>/trials (auto-detect study name)
    if path_parts.len() >= 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "trials" {
        let breeder_id = path_parts[2].to_string();
        let study_name = format!("{}_study", breeder_id);
        let query = parse_query(req.uri());
        let offset: i64 = query.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);

        return match state.optuna.get_trials(&breeder_id, &study_name, offset, limit).await {
            Ok(trials) => {
                let json = serde_json::json!({
                    "breeder_id": breeder_id,
                    "study_name": study_name,
                    "offset": offset,
                    "limit": limit,
                    "trials": trials,
                });
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to load trials: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        };
    }

    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("godon observer: try /metrics, /dashboard, /api/breeders/<uuid>/trials"))
        .unwrap())
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>godon observer</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:'SF Mono','Fira Code','Consolas',monospace;background:#0d1117;color:#c9d1d9;font-size:13px}
a{color:#58a6ff;text-decoration:none}
header{background:#161b22;border-bottom:1px solid #30363d;padding:10px 20px;display:flex;align-items:center;justify-content:space-between}
header h1{font-size:16px;color:#e6edf3}header h1 span{color:#3fb950}
.bar{background:#161b22;border-bottom:1px solid #30363d;padding:8px 20px;display:flex;align-items:center;gap:12px}
.bar label{color:#8b949e;min-width:50px}
.bar input[type="text"]{background:#0d1117;border:1px solid #30363d;color:#c9d1d9;padding:3px 8px;border-radius:3px;font-family:inherit;width:280px}
.bar button{background:#238636;color:#fff;border:none;padding:3px 10px;border-radius:3px;cursor:pointer;font-family:inherit}
.bar button:hover{background:#2ea043}
nav{background:#161b22;border-bottom:1px solid #30363d;padding:0 20px;display:flex}
nav button{background:none;border:none;border-bottom:2px solid transparent;color:#8b949e;padding:8px 14px;cursor:pointer;font-family:inherit;font-size:12px}
nav button.active{color:#e6edf3;border-bottom-color:#3fb950}
nav button:hover{color:#c9d1d9}
.top-info{padding:8px 20px;display:flex;gap:16px;font-size:12px;border-bottom:1px solid #21262d;background:#0d1117}
.top-info .tag{padding:2px 8px;border-radius:3px;font-size:11px}
.tag-green{background:#1a2a1a;color:#3fb950;border:1px solid #238636}
.tag-red{background:#2a1a1a;color:#f85149;border:1px solid #da3633}
.tag-blue{background:#1a1a2a;color:#58a6ff;border:1px solid #1f6feb}
.content{padding:16px 20px;overflow-y:auto;height:calc(100vh - 160px)}
canvas{border-radius:4px}
.legend{display:flex;gap:16px;margin-bottom:10px;font-size:11px}
.legend span{display:flex;align-items:center;gap:4px}
.legend .dot{width:10px;height:10px;border-radius:2px}
.slider-row{display:flex;align-items:center;gap:8px;margin:8px 0;font-size:12px;color:#8b949e}
.slider-row input[type="range"]{flex:1}
.slider-row .trial-info{color:#e6edf3;min-width:180px}
.heatmap-wrap{overflow-x:auto;padding-bottom:8px}
.heatmap-wrap canvas{min-width:100%}
.tooltip{position:fixed;background:#1c2128;border:1px solid #30363d;border-radius:6px;padding:8px 12px;font-size:11px;pointer-events:none;z-index:100;max-width:300px;display:none}
.error{background:#2a1a1a;border:1px solid #da3633;border-radius:4px;padding:8px;color:#f85149;margin:8px 0}
.info{background:#1a2a1a;border:1px solid #238636;border-radius:4px;padding:8px;color:#3fb950;margin:8px 0}
.guardrail-bar{display:flex;gap:8px;margin:8px 0;flex-wrap:wrap}
.guardrail-tag{padding:2px 8px;border-radius:3px;font-size:11px}
.status{position:fixed;bottom:0;left:0;right:0;background:#161b22;border-top:1px solid #30363d;padding:4px 20px;font-size:10px;color:#8b949e;display:flex;justify-content:space-between}
</style>
</head>
<body>
<header><h1>godon <span>observer</span></h1><span id="clock" style="font-size:11px;color:#8b949e"></span></header>
<div class="bar">
<label>breeder:</label><input type="text" id="breederId" placeholder="paste breeder UUID">
<button id="loadBtn">load</button>
<span id="loadStatus" style="color:#8b949e;font-size:11px"></span>
</div>
<div class="top-info" id="topInfo"></div>
<nav>
<button class="active" data-view="heatmap">heatmap</button>
<button data-view="spider">spider web</button>
<button data-view="parallel">parallel coordinates</button>
</nav>
<div class="content" id="content"><div class="info">paste a breeder UUID and click load</div></div>
<div class="tooltip" id="tooltip"></div>
<div class="status"><span id="statusL">ready</span><span id="statusR"></span></div>
<script>
const $=id=>document.getElementById(id);
let breederConfig=null,trials=[],currentView='heatmap',baseUrl='',hoverTrial=null;

function status(m){$('statusL').textContent=m}

async function api(path){
  const r=await fetch(baseUrl+path);
  if(!r.ok) throw new Error(r.status+' '+await r.text());
  return r.json();
}

function trialValue(t,objIdx){
  if(!t.values||t.state!=='COMPLETE') return null;
  const v=t.values[objIdx];
  return v===null||v===undefined?null:(v===Infinity?Infinity:(v===-Infinity?-Infinity:v));
}

function primaryValue(t){
  if(!breederConfig||!t.values) return null;
  const idx=breederConfig.objectives.findIndex(o=>o.direction==='maximize');
  return trialValue(t,idx>=0?idx:0);
}

function valueQuality(val,objIdx){
  if(val===null||val===undefined) return 0;
  if(!breederConfig) return 0.5;
  const obj=breederConfig.objectives[objIdx];
  if(!obj) return 0.5;
  if(obj.direction==='maximize') return val;
  return 1-val;
}

function trialQuality(t){
  if(!breederConfig) return primaryValue(t)||0;
  let sum=0,cnt=0;
  breederConfig.objectives.forEach((_,i)=>{const v=trialValue(t,i);if(v!==null&&v!==undefined){sum+=valueQuality(v,i);cnt++}});
  return cnt?sum/cnt:0;
}

function checkGuardrails(t){
  if(!breederConfig||!breederConfig.guardrails) return{};
  const violated={};
  breederConfig.guardrails.forEach(g=>{
    const param=t.params[g.name];
    if(param!==undefined&&g.hard_limit!==undefined&&param>g.hard_limit) violated[g.name]={value:param,limit:g.hard_limit};
  });
  return violated;
}

async function loadBreeder(){
  const bid=$('breederId').value.trim();
  if(!bid){alert('enter a breeder UUID');return}
  baseUrl=window.location.origin;
  $('loadStatus').textContent='loading...';
  $('topInfo').innerHTML='';
  try{
    breederConfig=await api('/api-proxy/breeders/'+bid);
    const studies=await api('/api/breeders/'+bid+'/studies');
    if(!studies.studies||!studies.studies.length){throw new Error('no studies found for breeder')}
    const studyName=studies.studies[0].study_name;
    breederConfig._studyName=studyName;
    breederConfig._directions=studies.studies[0].directions;
    let all=[],offset=0,limit=500;
    while(true){
      const batch=await api('/api/breeders/'+bid+'/trials/'+encodeURIComponent(studyName)+'?offset='+offset+'&limit='+limit);
      all=all.concat(batch.trials);
      if(batch.trials.length<limit) break;
      offset+=limit;
    }
    trials=all.filter(t=>t.state==='COMPLETE');
    breederConfig.objectives.forEach((obj,i)=>{
      if(breederConfig._directions&&breederConfig._directions[i]) obj.direction=breederConfig._directions[i].toLowerCase();
    });
    renderTopInfo();
    renderView();
    $('loadStatus').textContent=trials.length+' trials loaded';
    status('loaded');
  }catch(e){
    $('loadStatus').textContent='';
    $('content').innerHTML='<div class="error">'+e.message+'</div>';
    status('error: '+e.message);
  }
}

function renderTopInfo(){
  if(!breederConfig) return;
  const el=$('topInfo');
  const objTags=(breederConfig.objectives||[]).map((o,i)=>{
    const best=trials.reduce((b,t)=>{const v=trialValue(t,i);return v!==null&&(b===null||(o.direction==='maximize'?v>b:v<b))?v:b},null);
    return '<span class="tag tag-blue">'+o.name+': '+(best!==null?best.toFixed(4):'-')+' ('+o.direction+')</span>';
  }).join('');
  const grTags=(breederConfig.guardrails||[]).map(g=>{
    const hit=trials.some(t=>{const v=t.params[g.name];return v!==undefined&&v>g.hard_limit});
    return '<span class="guardrail-tag '+(hit?'tag-red':'tag-green')+'">'+g.name+(hit?' VIOLATED':' ok')+' &le; '+g.hard_limit+'</span>';
  }).join('');
  el.innerHTML=objTags+' <span style="color:#484f58">|</span> '+grTags;
}

function renderView(){
  const el=$('content');
  if(!trials.length){el.innerHTML='<div class="info">no completed trials</div>';return}
  if(currentView==='heatmap') renderHeatmap(el);
  else if(currentView==='spider') renderSpider(el);
  else if(currentView==='parallel') renderParallel(el);
}

function qualityColor(q){
  if(q===null||q===undefined) return '#30363d';
  const r=Math.floor(50+200*(1-q)),g=Math.floor(80+175*q);
  return 'rgb('+r+','+g+',60)';
}

function renderHeatmap(el){
  if(!breederConfig){el.innerHTML='<div class="info">no config</div>';return}
  const objs=breederConfig.objectives||[];
  const params=trials.length?Object.keys(trials[0].params):[];
  const rows=[...objs.map(o=>({label:o.name,type:'obj',idx:objs.indexOf(o)})),...params.map(p=>({label:p,type:'param'}))];
  const rowH=28,colW=36,padL=140,padT=30;
  const w=padL+trials.length*colW+40;
  const h=padT+rows.length*rowH+30;
  el.innerHTML='<div class="legend"><span><div class="dot" style="background:#3fb950"></div>good</span><span><div class="dot" style="background:#f85149"></div>bad</span><span><div class="dot" style="background:#30363d"></div>missing</span><span style="color:#8b949e">hover for details</span></div><div class="heatmap-wrap"><canvas id="hm" width="'+w+'" height="'+h+'"></canvas></div>';
  const c=$('hm').getContext('2d');
  c.fillStyle='#0d1117';c.fillRect(0,0,w,h);
  c.fillStyle='#8b949e';c.font='11px monospace';c.textAlign='right';
  rows.forEach((r,i)=>{
    const y=padT+i*rowH;
    const lbl=r.label.length>18?r.label.substring(0,16)+'..':r.label;
    c.fillText(lbl,padL-6,y+rowH/2+4);
    trials.forEach((t,ti)=>{
      const x=padL+ti*colW;
      let val,q;
      if(r.type==='obj'){
        val=trialValue(t,r.idx);
        q=valueQuality(val,r.idx);
      }else{
        val=t.params[r.label];
        q=trialQuality(t);
      }
      c.fillStyle=(val===null||val===undefined)?'#30363d':qualityColor(q);
      c.fillRect(x+1,y+1,colW-2,rowH-2);
      if(val!==null&&val!==undefined){
        c.fillStyle='rgba(0,0,0,0.7)';c.font='8px monospace';c.textAlign='center';
        c.fillText(val.toFixed(2),x+colW/2,y+rowH/2+3);
      }
    });
  });
  c.fillStyle='#8b949e';c.font='9px monospace';c.textAlign='center';
  trials.forEach((t,i)=>{c.fillText('T'+t.number,padL+i*colW+colW/2,padT+rows.length*rowH+14)});
  if(breederConfig.guardrails){
    breederConfig.guardrails.forEach(g=>{
      const pi=params.indexOf(g.name);
      if(pi<0) return;
      const y=padT+(objs.length+pi)*rowH+rowH/2;
      c.strokeStyle='#f85149';c.lineWidth=1;c.setLineDash([3,2]);
      c.beginPath();c.moveTo(padL,y);c.lineTo(padL+trials.length*colW,y);c.stroke();
      c.setLineDash([]);
    });
  }
  $('hm').onmousemove=e=>{
    const rect=$('hm').getBoundingClientRect();
    const mx=e.clientX-rect.left,my=e.clientY-rect.top;
    const ti=Math.floor((mx-padL)/colW),ri=Math.floor((my-padT)/rowH);
    if(ti>=0&&ti<trials.length&&ri>=0&&ri<rows.length){
      const t=trials[ti],r=rows[ri];
      const tip=$('tooltip');
      let html='<b>T'+t.number+'</b> ('+t.datetime_start?.substring(11,19)+')<br>';
      if(r.type==='obj'){
        const v=trialValue(t,r.idx);html+=r.label+': '+(v!==null?v.toFixed(4):'-')+'<br>';
      }
      html+='<br><b>params:</b>';
      Object.entries(t.params).forEach(([k,v])=>{html+='<br>'+k+': '+v.toFixed(3)});
      const gv=checkGuardrails(t);
      if(Object.keys(gv).length){html+='<br><br><b style="color:#f85149">violations:</b>';Object.entries(gv).forEach(([k,v])=>{html+='<br>'+k+': '+v.value.toFixed(2)+' > '+v.limit})}
      tip.innerHTML=html;tip.style.display='block';
      tip.style.left=(e.clientX+12)+'px';tip.style.top=(e.clientY+12)+'px';
    }else{$('tooltip').style.display='none'}
  };
  $('hm').onmouseleave=()=>{$('tooltip').style.display='none'};
}

function renderSpider(el){
  if(!breederConfig||!trials.length){el.innerHTML='<div class="info">no data</div>';return}
  const objs=breederConfig.objectives.map((o,i)=>({label:o.name,type:'obj',idx:i}));
  const params=Object.keys(trials[0].params).map(p=>({label:p,type:'param'}));
  const axes=[...objs,...params];
  if(axes.length<3){el.innerHTML='<div class="info">need 3+ axes</div>';return}
  const qColors=trials.map(t=>qualityColor(trialQuality(t)));
  el.innerHTML='<div class="legend"><span><div class="dot" style="background:#3fb950"></div>best</span><span style="color:#8b949e">background: all trials colored by quality</span></div><div class="slider-row"><label>trial:</label><input type="range" id="spSlider" min="0" max="'+(trials.length-1)+'" value="'+(trials.length-1)+'"><span class="trial-info" id="spInfo">'+trials.length+'/'+trials.length+'</span></div><canvas id="sp" width="600" height="500"></canvas>';
  const slider=$('spSlider');
  const draw=()=>{
    const idx=parseInt(slider.value);
    $('spInfo').textContent=(idx+1)+'/'+trials.length;
    const c=$('sp').getContext('2d');
    const cx=300,cy=240,R=180,n=axes.length;
    c.fillStyle='#0d1117';c.fillRect(0,0,600,500);
    for(let ring=1;ring<=4;ring++){const r=R*ring/4;c.strokeStyle='#21262d';c.beginPath();for(let i=0;i<=n;i++){const a=Math.PI*2*i/n-Math.PI/2;const x=cx+Math.cos(a)*r,y=cy+Math.sin(a)*r;i===0?c.moveTo(x,y):c.lineTo(x,y)}c.stroke()}
    for(let i=0;i<n;i++){const a=Math.PI*2*i/n-Math.PI/2;c.strokeStyle='#30363d';c.beginPath();c.moveTo(cx,cy);c.lineTo(cx+Math.cos(a)*R,cy+Math.sin(a)*R);c.stroke();c.fillStyle='#c9d1d9';c.font='10px monospace';c.textAlign='center';c.fillText(axes[i].label.length>14?axes[i].label.substring(0,12)+'..':axes[i].label,cx+Math.cos(a)*(R+20),cy+Math.sin(a)*(R+20)+4)}
    trials.forEach((t,ti)=>{
      if(ti===idx) return;
      c.strokeStyle=qColors[ti]+'44';c.lineWidth=1;c.beginPath();
      axes.forEach((ax,ai)=>{let val;if(ax.type==='obj'){val=trialValue(t,ax.idx)}else{val=t.params[ax.label]}const norm=val!==null&&val!==undefined?Math.min(Math.abs(val)/(Math.max(...trials.map(tt=>{if(ax.type==='obj'){const v=trialValue(tt,ax.idx);return v!==null?Math.abs(v):0}else{return Math.abs(tt.params[ax.label]||0)}}))||1,1.2):0;const ang=Math.PI*2*ai/n-Math.PI/2;c.lineTo(cx+Math.cos(ang)*R*norm,cy+Math.sin(ang)*R*norm)});c.closePath();c.stroke()
    });
    const t=trials[idx];c.strokeStyle=qColors[idx];c.lineWidth=2;c.beginPath();
    axes.forEach((ax,ai)=>{let val;if(ax.type==='obj'){val=trialValue(t,ax.idx)}else{val=t.params[ax.label]}const norm=val!==null&&val!==undefined?Math.min(Math.abs(val)/(Math.max(...trials.map(tt=>{if(ax.type==='obj'){const v=trialValue(tt,ax.idx);return v!==null?Math.abs(v):0}else{return Math.abs(tt.params[ax.label]||0)}}))||1,1.2):0;const ang=Math.PI*2*ai/n-Math.PI/2;c.lineTo(cx+Math.cos(ang)*R*norm,cy+Math.sin(ang)*R*norm)});c.closePath();c.stroke();c.fillStyle=qColors[idx]+'33';c.fill();
    const gv=checkGuardrails(t);const gvText=Object.keys(gv).length?' | <span style="color:#f85149">VIOLATED: '+Object.keys(gv).join(', ')+'</span>':'';
    c.fillStyle='#e6edf3';c.font='12px monospace';c.textAlign='center';
    c.fillText('T'+t.number+' — '+t.datetime_start?.substring(11,19)+gvText,cx,20);
  };
  slider.oninput=draw;draw();
}

function renderParallel(el){
  if(!breederConfig||!trials.length){el.innerHTML='<div class="info">no data</div>';return}
  const objs=breederConfig.objectives.map((o,i)=>({label:o.name,type:'obj',idx:i}));
  const params=Object.keys(trials[0].params).map(p=>({label:p,type:'param'}));
  const axes=[...objs,...params];
  const left=100,top=40,right=780,bottom=400;
  const spacing=(right-left)/(axes.length-1);
  el.innerHTML='<div class="legend"><span><div class="dot" style="background:#3fb950"></div>good trial</span><span><div class="dot" style="background:#f85149"></div>bad trial</span></div><canvas id="pc" width="900" height="460"></canvas>';
  const c=$('pc').getContext('2d');c.fillStyle='#0d1117';c.fillRect(0,0,900,460);
  axes.forEach((ax,i)=>{
    const x=left+i*spacing;
    c.strokeStyle='#30363d';c.lineWidth=1;c.beginPath();c.moveTo(x,top);c.lineTo(x,bottom);c.stroke();
    c.fillStyle='#c9d1d9';c.font='10px monospace';c.textAlign='center';
    c.fillText(ax.label.length>16?ax.label.substring(0,14)+'..':ax.label,x,bottom+16);
    const vals=trials.map(t=>{if(ax.type==='obj'){const v=trialValue(t,ax.idx);return v!==null?v:NaN}else return t.params[ax.label]||0}).filter(v=>!isNaN(v));
    if(vals.length){
      const mn=Math.min(...vals),mx=Math.max(...vals);
      c.fillStyle='#484f58';c.fillText(mx.toFixed(2),x,top-8);c.fillText(mn.toFixed(2),x,bottom+30);
    }
    if(ax.type==='param'){
      const g=breederConfig.guardrails?.find(g=>g.name===ax.label);
      if(g){const gy=bottom-((g.hard_limit-mn)/(mx-mn))*(bottom-top);c.strokeStyle='#f85149';c.lineWidth=1;c.setLineDash([3,2]);c.beginPath();c.moveTo(x-10,gy);c.lineTo(x+10,gy);c.stroke();c.setLineDash([])}
    }
  });
  const mn=axes.map(ax=>{const vs=trials.map(t=>{if(ax.type==='obj'){const v=trialValue(t,ax.idx);return v!==null?v:NaN}else return t.params[ax.label]||0}).filter(v=>!isNaN(v));return Math.min(...vs)});
  const mx=axes.map(ax=>{const vs=trials.map(t=>{if(ax.type==='obj'){const v=trialValue(t,ax.idx);return v!==null?v:NaN}else return t.params[ax.label]||0}).filter(v=>!isNaN(v));return Math.max(...vs)});
  trials.forEach((t,ti)=>{
    const q=trialQuality(t);c.strokeStyle=qualityColor(q);c.lineWidth=q>0.7?2:1;
    c.beginPath();
    axes.forEach((ax,ai)=>{
      let val;if(ax.type==='obj') val=trialValue(t,ax.idx);else val=t.params[ax.label];
      if(val===null||val===undefined) return;
      const range=mx[ai]-mn[ai]||1;
      const y=bottom-((val-mn[ai])/range)*(bottom-top);
      const x=left+ai*spacing;
      ai===0?c.moveTo(x,y):c.lineTo(x,y);
    });c.stroke()
  });
}

$('loadBtn').onclick=loadBreeder;
document.querySelectorAll('nav button').forEach(btn=>{btn.onclick=()=>{document.querySelectorAll('nav button').forEach(b=>b.classList.remove('active'));btn.classList.add('active');currentView=btn.dataset.view;renderView()}});
setInterval(()=>{$('clock').textContent=new Date().toLocaleTimeString()},1000);
</script>
</body>
</html>"##;

#[tokio::main]
async fn main() {
    let args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&args.log_level))
        .init();

    info!("Starting godon-observer v{}", env!("CARGO_PKG_VERSION"));
    info!("Push Gateway: {}", args.push_gateway_url);

    let state = Arc::new(ObserverState::new(args.push_gateway_url.clone(), args.api_url.clone()));
    let addr = format!("{}:{}", args.host, args.port);
    let addr = addr.parse().unwrap();

    let make_svc = make_service_fn(move |_| {
        let state = state.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req| handle_request(req, state.clone())))
        }
    });

    let server = Server::bind(&addr).serve(make_svc);

    info!("Observer listening on http://{}", addr);
    info!("  /metrics   - Prometheus metrics");
    info!("  /dashboard - Visualization dashboard");
    info!("  /api/breeders/<uuid>/trials - Trial history");

    if let Err(e) = server.await {
        error!("Server error: {}", e);
        std::process::exit(1);
    }
}
