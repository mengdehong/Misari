'use strict';
const __weCreated=[];
let __weLayerOrder=[];
function __weSerialize(data){return JSON.stringify(data,(_key,value)=>{if(typeof value==='number'&&!Number.isFinite(value))throw new RangeError('Non-finite SceneScript property');return value;});}
function __wePublishOrder(){if(__weSceneIndex>=0)__weSettings().__layerOrder=__weLayerOrder.slice();}
function __weCreateLayer(configuration) {
    if(__weNodes.length>=4096)throw new RangeError('Scene object budget exceeded');
    let config=configuration;
    if(config&&typeof config==='object') {
        config={...config};
        if(config.angles instanceof WEVec)config.angles=config.angles.multiply(Math.PI/180);
        if(config.parent&&typeof config.parent==='object')config.parent=config.parent.id;
        if(config.model instanceof IModelData)config.model={__wallpaperd_model_data:__weModelId(config.model)};
    }
    const index=__weNodes.length,owner=__weCurrent,overrides=__weOverrides;
    const prepared=JSON.parse(__wePrepareLayer(__weSerialize(config),__weSerialize(__weSettings()),__weSerialize(engine.userProperties),index,engine.runtime));
    const raw=prepared.values;
    if(raw.__texture){raw.__texture.position=0;raw.__texture.anchor=engine.runtime;raw.__texture.joined=false;}
    if(raw.__videoMaster!==undefined){
        const previous=[...__weVideoStates].find(([master,s])=>(s.key??s.asset)===(raw.__texture.key??raw.__texture.asset)&&__weNodes.some(n=>!n.__destroyed&&n.__videoMaster===master));
        raw.__videoMaster=previous?.[0]??index;
    }
    __weInitial[index]=prepared.definition;
    const node=__weMakeLayer(raw,index);__weNodes.push(node);__weLayerOrder.push(index);
    __weCreated.push([index,prepared.definition]);__wePublishOrder();
    const scripts=[];
    try {
        for(const binding of prepared.bindings){
            const slot=__weReserve(index,binding.path,binding.properties);
            const namespace=__weCompileCreated(binding.source);
            __weAttach(namespace,index,binding.path,binding.properties,slot);scripts.push(slot);
        }
        for(const slot of scripts)__weCall(slot,'applyGeneralSettings',{language:'en-us'});
        for(const slot of scripts)__weCall(slot,'init');
        for(const slot of scripts)__weCall(slot,'applyUserProperties',engine.userProperties);
        return node;
    } catch(error) {node.__pendingDestroy=true;throw error;}
    finally {__weOverrides=overrides;if(owner>=0)__weActivate(owner);else __weCurrent=owner;}
}
