'use strict';
function __weAnimationFor(index,scope,name){
    __weCheck(index);
    const current=__weScripts[__weCurrent]?.path;
    let info=__weNodes[index].__animations?.find(a=>name===undefined?JSON.stringify(a.path)===JSON.stringify(current):a.name===name&&JSON.stringify(a.path.slice(0,-1))===JSON.stringify(scope));
    if(!info)return undefined;
    return __weAnimationHandle(index,info);
}
function __weAnimationHandle(index,info){
    let owner=info;
    while(owner.parent!==null&&owner.parent!==undefined)owner=__weNodes[index].__animations.find(a=>a.index===owner.parent);
    const check=()=>__weCheck(index);
    const seek=frame=>{check();if(!Number.isFinite(frame)||Math.abs(frame)>1e12)throw new RangeError('Invalid animation frame');owner.__frame=frame;owner.__time=engine.runtime;owner.__revision++;
        for(const target of __weNodes[index].__animations){let leader=target;while(leader.parent!==null&&leader.parent!==undefined)leader=__weNodes[index].__animations.find(a=>a.index===leader.parent);if(leader.index!==owner.index)continue;
            let value=JSON.parse(__weSampleAnimation(target.index,frame));
            if(target.path.length===1&&target.path[0]==='angles')value=__weVector(value,3).multiply(180/Math.PI);
            const current=__wePath(__weNodes[index],target.path);
            if(current instanceof WEVec){value=__weVector(value,current._n);for(const component of ['x','y','z','w'].slice(0,current._n))current[component]=value[component];}
            else __weSetPath(__weNodes[index],target.path,value);
            const length=target.frameCount;
            if(length<=0)target.__current=0;else if(target.mode==='loop')target.__current=((frame%length)+length)%length;else if(target.mode==='mirror'){const x=((frame%(length*2))+length*2)%(length*2);target.__current=x>length?2*length-x:x;}else target.__current=Math.max(0,Math.min(length,frame));
        }
    };
    return Object.freeze({
        get fps(){check();return info.fps;},get frameCount(){check();return info.frameCount;},get duration(){check();return info.duration;},get name(){check();return info.name;},
        get rate(){check();return owner.rate;},set rate(rate){check();if(!Number.isFinite(rate)||Math.abs(rate)>128)throw new RangeError('Invalid animation rate');owner.rate=rate;},
        play(){check();if(!owner.playing||owner.mode==='single'&&owner.__current>=owner.frameCount)seek(0);owner.playing=true;owner.paused=false;},
        pause(){check();owner.paused=true;},stop(){check();seek(0);owner.playing=false;owner.paused=true;},
        isPlaying(){check();return owner.playing&&!owner.paused&&!(owner.mode==='single'&&owner.__current>=owner.frameCount);},
        getFrame(){check();return info.__current;},setFrame:seek,
    });
}
function __weAttachAnimations(value,index,scope){
    if(!value||typeof value!=='object'||Array.isArray(value)||value instanceof WEVec||value instanceof WEMatrix||Object.hasOwn(value,'getAnimation'))return;
    Object.defineProperty(value,'getAnimation',{value:name=>__weAnimationFor(index,scope,name),configurable:true});
}
function __weSyncValues(patches){
    __weSyncing=true;
    try {for(const [node,path,raw] of patches){
        if(__weNodes[node].__destroyed){if(path.length===1&&path[0]==='__texture'&&__weVideoStates.has(node))__weVideoStates.set(node,raw);continue;}
        let value=raw;const target=__wePath(__weNodes[node],path);
        if(target instanceof WEVec){
            const scale=path.length===1&&path[0]==='angles'?180/Math.PI:1;
            // Animation samples arrive as numeric arrays. Update the retained
            // vector directly without constructing temporary vectors or keys.
            if(Array.isArray(raw)&&raw.length>1&&(typeof raw[0]==='number'||raw[0]===null)){
                for(let i=0;i<target._n;i++)target[__weKeys[i]]=Number(raw[i]??0)*scale;
            }else{
                value=__weVector(value,target._n);
                if(scale!==1)value=value.multiply(scale);
                for(let i=0;i<target._n;i++)target[__weKeys[i]]=value[__weKeys[i]];
            }
        }
        else __weSetPath(__weNodes[node],path,value);
    }}finally{__weSyncing=false;}
}
