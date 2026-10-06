//! SceneScript image/model handles use detached matrices and checked bone indices.
const __weModelPoses=new Map();
function __weMat(values){const m=new Mat4();m.m=Array.from(values);return m;}
function __weLayerWorld(index){
    const chain=[],seen=new Set();let node=__weNodes[index];
    while(node){if(seen.has(node.__index))throw new RangeError('Layer parent cycle');seen.add(node.__index);chain.push(node);node=thisScene.getLayerByID(node.parent);}
    let world=Mat4.identity();
    let parent;
    for(const layer of chain.reverse()){
        if(parent&&layer.attachment!==undefined&&layer.attachment!==null&&layer.attachment!=='')world=world.multiply(__weAttachmentLocal(parent.__index,layer.attachment));
        world=world.multiply(Mat4.compose(layer.origin,layer.angles,layer.scale));parent=layer;
    }
    return world;
}
function __weBoneLocal(index,i){const raw=__weNodes[index],pose=__weModelPoses.get(index);const local=pose?.local?.[i]??(pose?__weBonePose(index,i):undefined);return __weMat((!local||raw.__boneOverrideTime?.[i]===engine.runtime?raw.__boneOverrides[i]:undefined)??local??raw.__model.bones[i].local);}
function __weBoneWorld(index,i){
    const bones=__weNodes[index].__model.bones,chain=[],seen=new Set();
    for(let p=i;p!==null&&p!==undefined;p=bones[p].parent){if(!bones[p]||seen.has(p))throw new RangeError('Invalid skeletal hierarchy');seen.add(p);chain.push(p);}
    let matrix=Mat4.identity();for(const bone of chain.reverse())matrix=matrix.multiply(__weBoneLocal(index,bone));return matrix;
}
function __weAttachmentLocal(index,key){
    __weCheck(index);const items=__weNodes[index].__model?.attachments??[];
    const slot=typeof key==='string'?items.findIndex(a=>a.name===key):key;
    if(!Number.isInteger(slot)||slot<0||slot>=items.length)throw new RangeError('Unknown attachment: '+key);
    const attachment=items[slot];return __weBoneWorld(index,attachment.bone).multiply(__weMat(attachment.matrix));
}
function __weSetParent(index,value,attachment,adjust=false){
    __weCheck(index);const node=__weNodes[index];
    const parent=value===undefined?undefined:typeof value==='object'?value:thisScene.getLayer(value)??thisScene.getLayerByID(value);
    if(value!==undefined&&(!parent||parent.__destroyed))throw new ReferenceError('Unknown parent layer');
    const seen=new Set([index]);
    for(let p=parent;p;p=thisScene.getLayerByID(p.parent)){if(seen.has(p.__index))throw new RangeError('Layer parent cycle');seen.add(p.__index);}
    const hasAttachment=attachment!==undefined&&attachment!==null&&attachment!=='';
    if(hasAttachment&&!parent)throw new RangeError('Attachment requires a parent');
    const parentMatrix=parent?(hasAttachment?parent.getAttachmentMatrix(attachment):parent.getTransformMatrix()):Mat4.identity();
    let pose;
    if(adjust){const local=parentMatrix.inverse().multiply(node.getTransformMatrix());pose=local.decompose();if(!Mat4.compose(pose.translation,pose.rotation,pose.scale).equals(local))throw new RangeError('Reparenting requires an unrepresentable shear');}
    node.parent=parent?.id??null;node.attachment=hasAttachment?attachment:null;
    if(pose){node.origin=pose.translation;node.angles=pose.rotation;node.scale=pose.scale;}
}
function __weInstallModel(raw,index){
    if(!raw.__model)return;
    raw.__boneOverrides??={};raw.__boneOverrideTime??={};raw.__bonePhysics??={};__weInstallModelAnimations(raw,index);
    const bones=raw.__model.bones;
    function boneIndex(value){__weCheck(index);const i=typeof value==='string'?bones.findIndex(b=>b.name===value):value;if(!Number.isInteger(i)||i<0||i>=bones.length)throw new RangeError('Unknown skeletal bone: '+value);return i;}
    function local(i){return __weBoneLocal(index,i);}
    function world(i){return __weBoneWorld(index,i);}
    function set(i,m){if(!(m instanceof Mat4)||m.m.length!==16||!m.m.every(Number.isFinite))throw new TypeError('Invalid bone transform');raw.__boneOverrides[i]=m.m.slice();raw.__boneOverrideTime[i]=engine.runtime;}
    function physics(bone,directional,angular,reset){
        __weCheck(index);
        const selected=bone===undefined?bones.map((_,i)=>i):[boneIndex(bone)];
        const vectors=[__weVector(directional??new Vec3(0),3),__weVector(angular??new Vec3(0),3)];
        if(vectors.some(v=>![v.x,v.y,v.z].every(n=>Number.isFinite(n)&&Math.abs(n)<=1e6)))throw new RangeError('Invalid bone physics impulse');
        for(const i of selected){
            const previous=raw.__bonePhysics[i];
            const same=previous?.time===engine.runtime;
            const direction=reset?new Vec3(0):vectors[0].add(same?new Vec3(previous.direction):new Vec3(0));
            const angle=reset?new Vec3(0):vectors[1].add(same?new Vec3(previous.angular):new Vec3(0));
            const revision=(previous?.revision??0)+1;
            raw.__bonePhysics[i]={revision,time:engine.runtime,resetRevision:reset?revision:(previous?.resetRevision??0),direction:[direction.x,direction.y,direction.z],angular:[angle.x,angle.y,angle.z]};
        }
    }
    Object.defineProperties(raw,{
        getBoneCount:{value:()=>{__weCheck(index);return bones.length;}},
        getBoneIndex:{value:name=>{__weCheck(index);return bones.findIndex(b=>b.name===name);}},
        getBoneParentIndex:{value:bone=>bones[boneIndex(bone)].parent??-1},
        getLocalBoneTransform:{value:bone=>local(boneIndex(bone))},
        setLocalBoneTransform:{value:(bone,m)=>set(boneIndex(bone),m)},
        getBoneTransform:{value:bone=>__weLayerWorld(index).multiply(world(boneIndex(bone)))},
        setBoneTransform:{value:(bone,m)=>{const i=boneIndex(bone),parent=bones[i].parent;let inverse=__weLayerWorld(index).inverse();if(parent!==null&&parent!==undefined)inverse=world(parent).inverse().multiply(inverse);set(i,inverse.multiply(m));}},
        getLocalBoneOrigin:{value:bone=>local(boneIndex(bone)).translation()},
        setLocalBoneOrigin:{value:(bone,v)=>{const i=boneIndex(bone),m=local(i);m.translation(__weVector(v,3));set(i,m);}},
        getLocalBoneAngles:{value:bone=>local(boneIndex(bone)).extractEuler()},
        setLocalBoneAngles:{value:(bone,v)=>{const i=boneIndex(bone),pose=local(i).decompose();set(i,Mat4.compose(pose.translation,__weVector(v,3),pose.scale));}},
        applyBonePhysicsImpulse:{value:(bone,directional,angular)=>physics(bone,directional,angular,false)},
        resetBonePhysicsSimulation:{value:bone=>physics(bone,undefined,undefined,true)},
        getAttachmentIndex:{value:name=>{__weCheck(index);return (raw.__model.attachments??[]).findIndex(a=>a.name===name);}},
        getAttachmentMatrix:{value:key=>__weLayerWorld(index).multiply(__weAttachmentLocal(index,key))},
        getAttachmentOrigin:{value:key=>raw.getAttachmentMatrix(key).translation()},
        getAttachmentAngles:{value:key=>raw.getAttachmentMatrix(key).extractEuler()},
    });
}
