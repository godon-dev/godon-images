mod optuna_reader;

use clap::Parser;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use log::{error, info, debug};
use prometheus::Encoder;
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
}

impl ObserverState {
    fn new(push_gateway_url: String) -> Self {
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

async fn handle_request(req: Request<Body>, state: Arc<ObserverState>) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path().to_string();
    let path_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    match path.as_str() {
        "/metrics" => {
            state.fetch_metrics();
            let metrics_text = state.get_metrics_text();
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
                .body(Body::from(metrics_text))
                .unwrap())
        }
        "/health" => {
            let db_ok = state.optuna.health_check().await;
            let body = if db_ok { "OK" } else { "DEGRADED: db unreachable" };
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(body))
                .unwrap())
        }
        "/dashboard" | "/dashboard/" => {
            Ok(html_response(DASHBOARD_HTML))
        }
        _ => {}
    }

    // /api/breeders/<uuid>/trials?offset=0&limit=100
    if path_parts.len() >= 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "trials" {
        let breeder_id = path_parts[2].to_string();
        let query: std::collections::HashMap<String, String> = req
            .uri()
            .query()
            .map(|q| urlencoding::decode(q).unwrap_or_default().into_owned())
            .map(|q| {
                q.split('&')
                    .filter_map(|pair| {
                        let mut kv = pair.split('=');
                        Some((kv.next()?.to_string(), kv.next()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let offset: i64 = query.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);

        let study_name = format!("{}_study", breeder_id);

        match state.optuna.get_trials(&breeder_id, &study_name, offset, limit).await {
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
        }
    }
    // /api/breeders/<uuid>/studies
    else if path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "studies" {
        let breeder_id = path_parts[2].to_string();
        match state.optuna.list_studies(&breeder_id).await {
            Ok(studies) => {
                let json = serde_json::json!({"breeder_id": breeder_id, "studies": studies});
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to list studies: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        }
    }
    // /api/breeders/<uuid>/summary
    else if path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "summary" {
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
        Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
    }
    // /api/breeders/<uuid>/trials/<study_name>/...
    else if path_parts.len() >= 5 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "trials" {
        let breeder_id = path_parts[2].to_string();
        let study_name = path_parts[4].to_string();

        let query: std::collections::HashMap<String, String> = req
            .uri()
            .query()
            .map(|q| {
                q.split('&')
                    .filter_map(|pair| {
                        let mut kv = pair.split('=');
                        Some((kv.next()?.to_string(), kv.next()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let offset: i64 = query.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);

        match state.optuna.get_trials(&breeder_id, &study_name, offset, limit).await {
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
        }
    } else {
        Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("godon observer: try /metrics, /dashboard, /api/breeders/<uuid>/trials"))
            .unwrap())
    }
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>godon observer</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:'SF Mono','Fira Code','Consolas',monospace;background:#0d1117;color:#c9d1d9}
header{background:#161b22;border-bottom:1px solid #30363d;padding:12px 24px;display:flex;align-items:center;justify-content:space-between}
header h1{font-size:18px;color:#e6edf3}header h1 span{color:#3fb950}
.config-bar{background:#161b22;border-bottom:1px solid #30363d;padding:8px 24px;display:flex;align-items:center;gap:12px;font-size:13px}
.config-bar label{color:#8b949e}
.config-bar input,.config-bar select{background:#0d1117;border:1px solid #30363d;color:#c9d1d9;padding:4px 8px;border-radius:4px;font-family:inherit;font-size:13px}
.config-bar input[type="text"]{width:300px}
.config-bar button{background:#238636;color:#fff;border:none;padding:4px 12px;border-radius:4px;cursor:pointer;font-family:inherit;font-size:13px}
nav{background:#161b22;border-bottom:1px solid #30363d;padding:0 24px;display:flex}
nav button{background:none;border:none;border-bottom:2px solid transparent;color:#8b949e;padding:10px 16px;cursor:pointer;font-family:inherit;font-size:13px}
nav button.active{color:#e6edf3;border-bottom-color:#3fb950}
.main{display:flex;height:calc(100vh - 130px)}
.sidebar{width:260px;background:#161b22;border-right:1px solid #30363d;overflow-y:auto;padding:12px}
.sidebar h3{font-size:11px;color:#8b949e;text-transform:uppercase;margin-bottom:8px}
.breeder-card{background:#0d1117;border:1px solid #30363d;border-radius:6px;padding:10px;margin-bottom:6px;cursor:pointer;font-size:12px}
.breeder-card:hover{border-color:#58a6ff}.breeder-card.selected{border-color:#3fb950}
.breeder-card .name{color:#e6edf3;font-weight:600;margin-bottom:4px}
.breeder-card .stat{color:#8b949e;display:flex;justify-content:space-between;margin-top:2px}.breeder-card .stat .val{color:#c9d1d9}
.content{flex:1;overflow-y:auto;padding:16px}
canvas{border-radius:6px}
.legend{display:flex;gap:16px;margin-bottom:12px;font-size:12px}
.legend span{display:flex;align-items:center;gap:4px}
.legend .dot{width:10px;height:10px;border-radius:2px}
.controls{display:flex;align-items:center;gap:12px;margin-bottom:12px;padding:8px 12px;background:#161b22;border-radius:6px;font-size:13px}
.controls label{color:#8b949e}.controls input[type="range"]{flex:1}
.error-msg{background:#3d1f1f;border:1px solid #f85149;border-radius:6px;padding:12px;color:#f85149;font-size:13px}
.info-msg{background:#1a2a1a;border:1px solid #3fb950;border-radius:6px;padding:12px;color:#3fb950;font-size:13px}
.status-bar{position:fixed;bottom:0;left:0;right:0;background:#161b22;border-top:1px solid #30363d;padding:6px 24px;font-size:11px;color:#8b949e;display:flex;justify-content:space-between}
</style>
</head>
<body>
<header><h1>godon <span>observer</span></h1><span id="clock" style="font-size:12px;color:#8b949e"></span></header>
<div class="config-bar">
<label>breeder:</label><input type="text" id="breederId" placeholder="breeder UUID">
<label>study:</label><input type="text" id="studyName" placeholder="auto-detected">
<label>observer:</label><input type="text" id="observerUrl" value="" style="width:200px">
<button id="loadBtn">load</button>
</div>
<nav>
<button class="active" data-view="heatmap">heatmap</button>
<button data-view="spider">spider web</button>
<button data-view="parallel">parallel coordinates</button>
</nav>
<div class="main">
<div class="sidebar">
<h3>breeder info</h3>
<div id="breederInfo"><div class="info-msg">enter a breeder UUID and click load</div></div>
</div>
<div class="content" id="content"></div>
</div>
<div class="status-bar"><span id="status">ready</span><span id="statusR"></span></div>
<script>
const $=id=>document.getElementById(id);
let trials=[],studyInfo=null,currentView='heatmap',baseUrl='';

function status(msg){$('status').textContent=msg}

async function api(path){
  const url=baseUrl+path;
  const r=await fetch(url);
  if(!r.ok) throw new Error(r.status+' '+await r.text());
  return r.json();
}

async function loadBreeder(){
  const bid=$('breederId').value.trim();
  if(!bid){alert('enter a breeder UUID');return}
  baseUrl=$('observerUrl').value.trim()||'';
  status('loading...');
  try{
    const summary=await api(`/api/breeders/${bid}/summary`);
    studyInfo=summary;
    let studyName=$('studyName').value.trim()||summary.study_name;
    let offset=0,limit=1000,all=[];
    while(true){
      const batch=await api(`/api/breeders/${bid}/trials/${studyName}?offset=${offset}&limit=${limit}`);
      all=all.concat(batch.trials);
      if(batch.trials.length<limit) break;
      offset+=limit;
    }
    trials=all;
    $('breederInfo').innerHTML=`
      <div class="breeder-card selected">
        <div class="name">${bid.substring(0,12)}...</div>
        <div class="stat"><span>total trials</span><span class="val">${summary.total_trials}</span></div>
        <div class="stat"><span>loaded</span><span class="val">${trials.length}</span></div>
        <div class="stat"><span>study</span><span class="val">${studyName}</span></div>
      </div>`;
    status(`loaded ${trials.length} trials`);
    renderView();
  }catch(e){
    status('error: '+e.message);
    $('breederInfo').innerHTML=`<div class="error-msg">${e.message}</div>`;
  }
}

function renderView(){
  const el=$('content');
  if(!trials.length){el.innerHTML='<div class="info-msg">no trials loaded</div>';return}
  if(currentView==='heatmap') renderHeatmap(el);
  else if(currentView==='spider') renderSpider(el);
  else if(currentView==='parallel') renderParallel(el);
}

function getTrialMetrics(t){
  const completed=t.state==='COMPLETE'&&t.values&&t.values.length>0;
  return{number:t.number,state:t.state,params:t.params,value:completed?t.values[0]:null,values:t.values};
}

function renderHeatmap(el){
  const completed=trials.filter(t=>t.state==='COMPLETE'&&t.values&&t.values.length>0);
  if(!completed.length){el.innerHTML='<div class="info-msg">no completed trials</div>';return}
  const metrics=['trial_value',...Object.keys(completed[0].params)];
  el.innerHTML=`<div class="legend"><span><div class="dot" style="background:#3fb950"></div>good</span><span><div class="dot" style="background:#f85149"></div>poor</span></div><canvas id="hm" width="${Math.max(800,completed.length*40)}" height="${Math.max(300,metrics.length*50)}"></canvas>`;
  const c=$('hm').getContext('2d');
  const rowH=45,colW=Math.max(30,700/completed.length);
  const left=120,top=30;
  const allVals=completed.map(t=>t.values[0]||0);
  const minV=Math.min(...allVals),maxV=Math.max(...allVals);
  metrics.forEach((m,mi)=>{
    const y=top+mi*rowH;
    c.fillStyle='#c9d1d9';c.font='11px monospace';c.textAlign='right';
    c.fillText(m.length>20?m.substring(0,18)+'..':m,left-6,y+rowH/2+4);
    completed.forEach((t,ti)=>{
      const x=left+ti*colW;
      let val= m==='trial_value'?(t.values[0]||0):(t.params[m]||0);
      let norm=m==='trial_value'?(val-minV)/(maxV-minV||1):val;
      let g=Math.floor(80+175*Math.abs(norm)),r=Math.floor(50+100*(1-Math.abs(norm)));
      c.fillStyle=`rgb(${r},${g},70)`;
      c.fillRect(x+1,y+1,colW-2,rowH-2);
      c.fillStyle='#0d1117';c.font='8px monospace';c.textAlign='center';
      c.fillText(val.toFixed(2),x+colW/2,y+rowH/2+3);
    });
  });
  c.fillStyle='#8b949e';c.font='9px monospace';c.textAlign='center';
  completed.forEach((t,i)=>{c.fillText('T'+t.number,left+i*colW+colW/2,top+metrics.length*rowH+14)});
}

function renderSpider(el){
  el.innerHTML=`<div class="legend"><span><div class="dot" style="background:#3fb950"></div>best</span><span><div class="dot" style="background:#58a6ff44"></div>all trials</span></div>
  <div class="controls"><label>trial:</label><input type="range" id="spSlider" min="0" max="${trials.length-1}" value="${trials.length-1}"><span id="spNum">${trials.length}/${trials.length}</span></div>
  <canvas id="sp" width="500" height="500"></canvas>`;
  const slider=$('spSlider');
  const drawSpider=()=>{
    const idx=parseInt(slider.value);
    $('spNum').textContent=`${idx+1}/${trials.length}`;
    const c=$('sp').getContext('2d');
    const cx=250,cy=250,R=180;
    const t=trials[idx];
    const completed=t.state==='COMPLETE'&&t.values;
    const axes=Object.keys(trials[0].params||{});
    if(completed&&t.values&&t.values.length>0) axes.push('objective');
    const n=axes.length;
    if(n<3){c.fillStyle='#f85149';c.font='14px monospace';c.fillText('need 3+ metrics for spider web',100,250);return}
    c.fillStyle='#0d1117';c.fillRect(0,0,500,500);
    for(let ring=1;ring<=4;ring++){const r=R*ring/4;c.strokeStyle='#21262d';c.beginPath();for(let i=0;i<=n;i++){const a=Math.PI*2*i/n-Math.PI/2;const x=cx+Math.cos(a)*r,y=cy+Math.sin(a)*r;i===0?c.moveTo(x,y):c.lineTo(x,y)}c.stroke()}
    for(let i=0;i<n;i++){const a=Math.PI*2*i/n-Math.PI/2;c.strokeStyle='#30363d';c.beginPath();c.moveTo(cx,cy);c.lineTo(cx+Math.cos(a)*R,cy+Math.sin(a)*R);c.stroke();c.fillStyle='#c9d1d9';c.font='10px monospace';c.textAlign='center';c.fillText(axes[i].length>15?axes[i].substring(0,13)+'..':axes[i],cx+Math.cos(a)*(R+22),cy+Math.sin(a)*(R+22)+4)}
    trials.forEach((tr,ti)=>{
      if(ti===idx)return;
      const vals=axes.map(a=>{const v=(tr.params||{})[a];return v!==undefined?v:(tr.values&&tr.values[0])||0});
      const maxes=axes.map(a=>{let m=0;trials.forEach(t=>{const v=(t.params||{})[a];if(v!==undefined&&v>m)m=v;if(a==='objective'&&t.values&&t.values[0]>m)m=t.values[0]});return m||1});
      c.strokeStyle='#58a6ff22';c.lineWidth=1;c.beginPath();
      axes.forEach((a,i)=>{const norm=vals[i]/maxes[i];const ang=Math.PI*2*i/n-Math.PI/2;c.lineTo(cx+Math.cos(ang)*R*Math.min(norm,1.2),cy+Math.sin(ang)*R*Math.min(norm,1.2))});
      c.closePath();c.stroke()
    });
    const vals=axes.map(a=>{const v=(t.params||{})[a];return v!==undefined?v:(t.values&&t.values[0])||0});
    const maxes=axes.map(a=>{let m=0;trials.forEach(t=>{const v=(t.params||{})[a];if(v!==undefined&&v>m)m=v;if(a==='objective'&&t.values&&t.values[0]>m)m=t.values[0]});return m||1});
    c.strokeStyle='#3fb950';c.lineWidth=2;c.beginPath();
    axes.forEach((a,i)=>{const norm=vals[i]/maxes[i];const ang=Math.PI*2*i/n-Math.PI/2;c.lineTo(cx+Math.cos(ang)*R*Math.min(norm,1.2),cy+Math.sin(ang)*R*Math.min(norm,1.2))});
    c.closePath();c.stroke();c.fillStyle='#3fb95022';c.fill();
    c.fillStyle='#e6edf3';c.font='12px monospace';c.textAlign='center';
    c.fillText(`trial ${t.number} — ${t.state} — objective: ${completed&&t.values?t.values[0]?.toFixed(4):'N/A'}`,cx,20);
  };
  slider.oninput=drawSpider;
  drawSpider();
}

function renderParallel(el){
  const completed=trials.filter(t=>t.state==='COMPLETE'&&t.values&&t.values.length>0);
  if(!completed.length){el.innerHTML='<div class="info-msg">no completed trials</div>';return}
  const axes=['objective',...Object.keys(completed[0].params)];
  const maxes=axes.map(a=>{let m=0;completed.forEach(t=>{const v=a==='objective'?(t.values[0]||0):(t.params[a]||0);if(v>m)m=v});return m||1});
  const mins=axes.map(a=>{let m=Infinity;completed.forEach(t=>{const v=a==='objective'?(t.values[0]||0):(t.params[a]||0);if(v<m)m=v});return m});
  el.innerHTML=`<div class="legend"><span><div class="dot" style="background:#3fb950"></div>best trial</span><span><div class="dot" style="background:#58a6ff44"></div>other trials</span></div><canvas id="pc" width="800" height="400"></canvas>`;
  const c=$('pc').getContext('2d');c.fillStyle='#0d1117';c.fillRect(0,0,800,400);
  const left=80,top=30,right=720,bottom=350,spacing=(right-left)/(axes.length-1);
  axes.forEach((a,i)=>{
    const x=left+i*spacing;c.strokeStyle='#30363d';c.lineWidth=1;c.beginPath();c.moveTo(x,top);c.lineTo(x,bottom);c.stroke();
    c.fillStyle='#c9d1d9';c.font='10px monospace';c.textAlign='center';
    c.fillText(a.length>18?a.substring(0,16)+'..':a,x,bottom+16);
    c.fillStyle='#8b949e';c.fillText(maxes[i].toFixed(1),x,top-8);c.fillText(mins[i].toFixed(1),x,bottom+30);
  });
  let bestIdx=0,bestVal=-Infinity;
  completed.forEach((t,i)=>{if(t.values&&t.values[0]>bestVal){bestVal=t.values[0];bestIdx=i}});
  completed.forEach((t,ti)=>{
    const isBest=ti===bestIdx;c.strokeStyle=isBest?'#3fb950':'#58a6ff33';c.lineWidth=isBest?2:1;
    c.beginPath();
    axes.forEach((a,ai)=>{
      const v=a==='objective'?(t.values[0]||0):(t.params[a]||0);
      const range=maxes[ai]-mins[ai]||1;
      const y=bottom-((v-mins[ai])/range)*(bottom-top);
      const x=left+ai*spacing;
      ai===0?c.moveTo(x,y):c.lineTo(x,y);
    });c.stroke()
  });
}

$('loadBtn').onclick=loadBreeder;
document.querySelectorAll('nav button').forEach(btn=>{
  btn.onclick=()=>{document.querySelectorAll('nav button').forEach(b=>b.classList.remove('active'));btn.classList.add('active');currentView=btn.dataset.view;renderView()}
});
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

    let state = Arc::new(ObserverState::new(args.push_gateway_url.clone()));
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
