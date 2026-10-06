function __weInstallParticle(raw,index){
    if(typeof raw.particle!=='string')return;
    raw.__particle??={playing:true,live:0,commands:[]};
    raw.instanceoverride??={};
    for(const key of ['alpha','size','count','speed','lifetime','rate'])raw.instanceoverride[key]??=1;
    const vector=value=>__weVector(typeof value==='string'?value.trim().split(/\s+/).map(Number):value,3);
    raw.instanceoverride.colorn=vector(raw.instanceoverride.colorn??[1,1,1]);
    for(let i=0;i<8;i++)raw.instanceoverride['controlpoint'+i]=vector(raw.instanceoverride['controlpoint'+i]??raw.__particle.controlpoints?.[i]??[0,0,0]);
    const state=()=>{__weCheck(index);return __weNodes[index].__particle;};
    const command=(action,count=0)=>{
        const s=state();
        if(s.commands.length>=128)throw new RangeError('Particle command budget exceeded');
        s.commands.push([action,count]);
        if(action==='stop'){s.playing=false;s.live=0;}
        if(action==='play'||action==='emit'&&count>0)s.playing=true;
        if(action==='pause')s.playing=s.live>0;
    };
    Object.defineProperties(raw,{
        instance:{get:()=>{__weCheck(index);return __weNodes[index].instanceoverride;}},
        isPlaying:{value:()=>state().playing},
        play:{value:()=>command('play')},
        pause:{value:()=>command('pause')},
        stop:{value:()=>command('stop')},
        emitParticles:{value:(count=1)=>{if(!Number.isInteger(count)||count<0||count>20000)throw new RangeError('Invalid particle count');command('emit',count);}},
    });
}
